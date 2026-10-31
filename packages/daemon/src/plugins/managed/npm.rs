use super::{command, read_json, reference::string};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::Path, sync::LazyLock};

static SOURCE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(
        r"^(?P<name>(?:@[a-z0-9][a-z0-9._-]*/)?[a-z0-9][a-z0-9._-]*)(?:@(?P<spec>[^:@/\\]+))?$",
    )
    .expect("constant npm grammar")
});

pub fn is_source(source: &str) -> bool {
    source.starts_with("npm:") || SOURCE.is_match(source)
}

pub fn parse(source: &str) -> Result<(&str, &str)> {
    let source = source.strip_prefix("npm:").unwrap_or(source);
    let captures = SOURCE.captures(source).context(
        "Invalid npm plugin source. Use npm:package or npm:@scope/package@version, tag, or range.",
    )?;
    let name = captures.name("name").expect("required npm name").as_str();
    let spec = captures
        .name("spec")
        .map(|m| m.as_str())
        .unwrap_or("latest");
    if spec.trim().is_empty() || spec != spec.trim() {
        bail!("An npm version, tag, or range must be non-empty without surrounding spaces");
    }
    Ok((name, spec))
}

async fn npm(args: Vec<String>, cwd: &Path, env: &BTreeMap<String, String>) -> Result<String> {
    let executable = which::which_in("npm", env.get("PATH"), cwd).context("npm is required on the daemon host to acquire or check npm plugins. Install Node.js with npm and make npm available on the daemon's PATH.")?;
    command(
        executable.to_string_lossy().as_ref(),
        args,
        cwd,
        env.clone(),
    )
    .await
}

pub async fn acquire(
    source: &str,
    root: &Path,
    target: Option<&Value>,
    env: &BTreeMap<String, String>,
) -> Result<String> {
    let (name, requested) = parse(source)?;
    let dependency = match target {
        Some(target) => string(target, "resolved")?,
        None => requested,
    };
    let package = json!({"name":"paseo-plugin-installation","version":"1.0.0","private":true,"dependencies":{(name):dependency}});
    tokio::fs::write(root.join("package.json"), serde_json::to_vec(&package)?).await?;
    npm(
        [
            "install",
            "--ignore-scripts",
            "--legacy-peer-deps",
            "--no-audit",
            "--no-fund",
            "--package-lock=true",
            "--lockfile-version=3",
            "--include=prod",
            "--omit=dev",
            "--global=false",
            "--workspaces=false",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect(),
        root,
        env,
    )
    .await?;
    let installed = artifact(root, name).await?;
    if let Some(target) = target {
        if ["version", "integrity", "resolved"]
            .iter()
            .any(|key| installed[*key] != target[*key])
        {
            bail!("npm artifact changed since review for {name}");
        }
    }
    Ok(name.to_owned())
}

pub async fn artifact(root: &Path, name: &str) -> Result<Value> {
    let lock = read_json(&root.join("package-lock.json")).await?;
    if lock["lockfileVersion"].as_u64().is_none_or(|v| v < 2) {
        bail!("Unsupported npm lockfile version");
    }
    let artifact = &lock["packages"][format!("node_modules/{name}")];
    let version = string(artifact, "version")?;
    let resolved = string(artifact, "resolved")?;
    let integrity = string(artifact, "integrity")?;
    let package_dir = root.join("node_modules").join(name);
    super::containment(root, &package_dir).await?;
    let installed = read_json(&package_dir.join("package.json")).await?;
    let package = read_json(&root.join("package.json")).await?;
    let dependencies = package["dependencies"]
        .as_object()
        .context("Missing npm acquisition dependencies")?;
    if dependencies.len() != 1
        || dependencies.get(name).and_then(Value::as_str).is_none()
        || installed["name"] != name
        || installed["version"] != version
    {
        bail!("Installed npm package does not match its acquisition artifacts: {name}");
    }
    Ok(json!({"version":version,"resolved":resolved,"integrity":integrity}))
}

pub async fn resolve(
    name: &str,
    selector: &str,
    cwd: &Path,
    env: &BTreeMap<String, String>,
) -> Result<Value> {
    let source = format!("npm:{name}@{selector}");
    let (parsed, _) = parse(&source)?;
    if parsed != name {
        bail!("Invalid npm package selector");
    }
    let raw = npm(
        vec![
            "view".into(),
            format!("{name}@{selector}"),
            "version".into(),
            "dist".into(),
            "--json".into(),
        ],
        cwd,
        env,
    )
    .await?;
    let value: Value = serde_json::from_str(&raw)?;
    let entries = if let Some(entries) = value.as_array() {
        entries.as_slice()
    } else {
        std::slice::from_ref(&value)
    };
    let mut best: Option<(&Value, semver::Version)> = None;
    for entry in entries {
        let version = semver::Version::parse(string(entry, "version")?)?;
        url::Url::parse(string(&entry["dist"], "tarball")?)?;
        string(&entry["dist"], "integrity")?;
        if best
            .as_ref()
            .is_none_or(|(_, previous)| version.cmp_precedence(previous).is_gt())
        {
            best = Some((entry, version));
        }
    }
    let (latest, _) = best.with_context(|| format!("No npm version matches {name}@{selector}"))?;
    Ok(
        json!({"kind":"npm","version":latest["version"],"resolved":latest["dist"]["tarball"],"integrity":latest["dist"]["integrity"]}),
    )
}
