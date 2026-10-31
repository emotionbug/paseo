use super::reference::{is_commit, plugin_path, registry_id, string};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

pub struct Resolved {
    pub input: Value,
    pub target: Value,
    pub artifact: Value,
}

pub async fn resolve(identity: &Value, options: &Value, install: bool) -> Result<Resolved> {
    let base = url::Url::parse(string(identity, "url")?)?;
    let id = string(identity, "id")?;
    if !registry_id(id) {
        bail!("Invalid plugin registry ID");
    }
    if !matches!(base.scheme(), "http" | "https")
        || !base.username().is_empty()
        || base.password().is_some()
    {
        bail!("Registry base must be an HTTP(S) URL without credentials");
    }
    let host = &base[url::Position::BeforeHost..url::Position::AfterPort];
    let authorization = options["pluginRegistries"][host]["authorization"].as_str();
    if authorization.is_some()
        && base.scheme() != "https"
        && !matches!(
            base.host_str(),
            Some("localhost" | "127.0.0.1" | "[::1]" | "::1")
        )
    {
        bail!("Registry credentials require HTTPS");
    }
    let url = format!(
        "{}/plugins/{id}.json",
        string(identity, "url")?.trim_end_matches('/')
    );
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(30))
        .build()?;
    let mut request = client.get(&url);
    if let Some(authorization) = authorization {
        request = request.header(reqwest::header::AUTHORIZATION, authorization);
    }
    if install {
        request = request.header("X-Paseo-Install", "1");
    }
    let mut response = request.send().await.map_err(|_| anyhow::anyhow!("Could not reach plugin registry {url}. {}", if install { format!("Plugin {id} was not installed. Retry or use an explicit npm: or github: source.") } else { format!("Could not check updates for {id}. Retry later.") }))?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        bail!(
            "Plugin {id} was not found in registry {host}. {}",
            if install {
                format!(
                    "If you intended a local directory, use ./{id}. If you intended a GitHub source, use git:{id}, or a full Git URL for another Git host."
                )
            } else {
                "Check that the installed plugin is still published in this registry before updating.".to_owned()
            }
        );
    }
    if !response.status().is_success() {
        bail!(
            "Registry {host} returned {} for {id}",
            response.status().as_u16()
        );
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if bytes.len() + chunk.len() > 4 * 1024 * 1024 {
            bail!("Plugin registry response exceeds 4 MiB");
        }
        bytes.extend_from_slice(&chunk);
    }
    let document: Value = serde_json::from_slice(&bytes).context("Invalid registry JSON")?;
    if document["id"] != id {
        bail!("Registry returned a different plugin ID");
    }
    let artifact = document
        .get("artifact")
        .context("Registry artifact is missing")?
        .clone();
    let (input, target) = match string(&artifact, "kind")? {
        "npm" => {
            let name = string(&artifact, "package")?;
            let version = string(&artifact, "version")?;
            let (parsed, _) = super::npm::parse(name)?;
            if parsed != name {
                bail!("Invalid registry npm package");
            }
            semver::Version::parse(version).context("Invalid registry npm version")?;
            let resolved = string(&artifact, "resolved")?;
            url::Url::parse(resolved)?;
            let integrity = string(&artifact, "integrity")?;
            let digest = integrity
                .strip_prefix("sha512-")
                .context("Registry npm integrity requires sha512")?;
            if digest.is_empty()
                || !digest
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"+/=".contains(&b))
            {
                bail!("Invalid registry npm integrity");
            }
            (
                json!({"source":format!("npm:{name}@{version}")}),
                json!({"kind":"npm","version":version,"resolved":resolved,"integrity":integrity}),
            )
        }
        "git" => {
            let remote = string(&artifact, "remote")?;
            let commit = string(&artifact, "commit")?;
            if !is_commit(commit) {
                bail!("Invalid registry Git commit");
            }
            let path = artifact
                .get("pluginPath")
                .map(|v| v.as_str().context("Invalid registry pluginPath"))
                .transpose()?;
            if path.is_some_and(|p| p.is_empty() || p.contains(['\\', ':'])) {
                bail!("Invalid registry pluginPath");
            }
            plugin_path(path)?;
            let mut input = json!({"source":format!("git:{remote}"),"ref":commit});
            if let Some(path) = path {
                input["pluginPath"] = json!(path);
            }
            (input, json!({"kind":"git","commit":commit}))
        }
        _ => bail!("Invalid registry artifact kind"),
    };
    Ok(Resolved {
        input,
        target,
        artifact,
    })
}

pub fn assert_identity(identity: &Value, artifact: &Value) -> Result<()> {
    let matches = if identity["kind"] == "npm" && artifact["kind"] == "npm" {
        identity["packageName"] == artifact["package"] && identity["pluginPath"] == "."
    } else if identity["kind"] == "git" && artifact["kind"] == "git" {
        identity["remote"].as_str()
            == Some(super::reference::redact(string(artifact, "remote")?).as_str())
            && identity["pluginPath"].as_str()
                == Some(artifact["pluginPath"].as_str().unwrap_or("."))
    } else {
        false
    };
    if !matches {
        bail!("Registry artifact source changed; reinstall to approve the new source");
    }
    Ok(())
}
