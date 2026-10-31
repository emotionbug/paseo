use super::*;
use std::path::{Path, PathBuf};
use tokio::io::AsyncWriteExt;

pub(super) fn command() -> Command {
    Command::new("plugin")
        .about("Manage trusted, unsandboxed plugins")
        .subcommand_required(true)
        .subcommands([
            Command::new("init")
                .about("Create a typecheckable server plugin")
                .arg(positional("directory", true))
                .arg(option("id", "id")),
            Command::new("ls")
                .alias("status")
                .arg(positional("id", false)),
            Command::new("logs").arg(positional("id", true)),
            Command::new("install")
                .alias("add")
                .arg(positional("source", true))
                .arg(option("id", "id"))
                .arg(option("ref", "ref"))
                .arg(option("path", "path")),
            Command::new("update")
                .arg(positional("id", false))
                .arg(switch("all", "all"))
                .arg(switch("check", "check"))
                .arg(switch("yes", "yes"))
                .arg(option("ref", "ref"))
                .arg(option("version", "version")),
            Command::new("reload").arg(positional("id", true)),
            Command::new("enable").arg(positional("id", true)),
            Command::new("disable").arg(positional("id", true)),
            Command::new("remove").arg(positional("id", true)),
        ])
}
fn identity(value: &Value) -> String {
    if value["kind"] == "directory" {
        return text(value, "path").into();
    }
    if value["registry"].is_object() {
        if let Ok(url) = url::Url::parse(text(&value["registry"], "url")) {
            return format!(
                "{}/{}",
                &url[url::Position::BeforeHost..url::Position::AfterPort],
                text(&value["registry"], "id")
            );
        }
    }
    let source = if value["kind"] == "npm" {
        format!("npm:{}", text(value, "packageName"))
    } else {
        format!("git:{}", text(value, "remote"))
    };
    if value["pluginPath"] == "." {
        source
    } else {
        format!("{source}:{}", text(value, "pluginPath"))
    }
}
fn plugin_output(value: Value, context: &Context) -> Result<Output> {
    if context.output.format != Format::Table || context.output.quiet {
        return Ok(Output::new(value));
    }
    let rows = value
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_else(|| std::slice::from_ref(&value));
    let display:Vec<Value>=rows.iter().map(|p|json!({"id":p["id"],"status":p["status"],"enabled":if p["enabled"]==true{"yes"}else{"no"},"source":if p["installation"].is_object(){identity(&p["installation"]["identity"])}else{"-".into()},"revision":p["installation"]["currentRevision"].as_str().unwrap_or("-"),"path":p["path"],"error":p["error"].as_str().unwrap_or("")})).collect();
    Ok(Output::new(value)
        .table_data(json!(display))
        .columns(&[
            ("PLUGIN", "id"),
            ("STATUS", "status"),
            ("ENABLED", "enabled"),
            ("SOURCE", "source"),
            ("REVISION", "revision"),
            ("DIRECTORY", "path"),
            ("ERROR", "error"),
        ])
        .widths(&[20, 10, 8, 40, 16, 40, 40]))
}
pub(super) async fn execute(
    command: &str,
    m: &ArgMatches,
    context: &Context,
    client: &DaemonClient,
) -> Result<Output> {
    let (feature, message) = match command {
        "logs" => ("pluginLogs", "Update the host to view plugin logs."),
        "install" | "add" => (
            "pluginSourceInstallation",
            "Update the host to install plugin sources.",
        ),
        "update" => (
            "pluginSourceUpdates",
            "Update the host to review plugin updates.",
        ),
        _ => (
            "pluginManagement",
            "Update the host to use plugin management.",
        ),
    };
    if client.server_info()["features"][feature] != true {
        return Err(error("DAEMON_UPDATE_REQUIRED", message));
    }
    match command {
        "ls" | "status" => {
            let payload = rpc(
                client,
                context,
                json!({"type":"plugin.list.request"}),
                "plugin.list.response",
            )
            .await?;
            let mut plugins = response_array(&payload, "plugins")?.to_vec();
            if let Some(id) = string(m, "id") {
                plugins.retain(|p| p["id"] == id);
                if plugins.is_empty() {
                    return Err(anyhow!("Plugin is not configured: {id}"));
                }
            }
            plugin_output(json!(plugins), context)
        }
        "logs" => {
            let payload = rpc(
                client,
                context,
                json!({"type":"plugin.logs.get.request","pluginId":required(m,"id")?}),
                "plugin.logs.get.response",
            )
            .await?;
            Ok(Output::new(json!(response_array(&payload, "entries")?))
                .id("sequence")
                .columns(&[
                    ("TIME", "timestamp"),
                    ("STREAM", "stream"),
                    ("MESSAGE", "message"),
                ])
                .widths(&[24, 8, 80]))
        }
        "install" | "add" => {
            eprintln!(
                "Trusting plugin code: backend code and preparation commands run unsandboxed on the daemon host. JavaScript backend extensions require external Node.js. Dependencies and future updates are part of the codebase you trust."
            );
            let source = source_reference(required(m, "source")?, string(m, "path"))?;
            let mut request = json!({"type":"plugin.source.install.request","source":source});
            for key in ["id", "ref"] {
                if let Some(value) = string(m, key).filter(|v| !v.is_empty()) {
                    request[key] = json!(value);
                }
            }
            let payload = client
                .request_cancellable(
                    request,
                    "plugin.source.install.response",
                    Duration::from_secs(300),
                    context.cancel.clone(),
                )
                .await?;
            plugin_output(payload["plugin"].clone(), context)
        }
        "update" => update(m, context, client).await,
        "reload" | "enable" | "disable" => {
            let payload=client.request_cancellable(json!({"type":format!("plugin.{command}.request"),"pluginId":required(m,"id")?}),&format!("plugin.{command}.response"),if command=="reload"{Duration::ZERO}else{Duration::from_secs(60)},context.cancel.clone()).await?;
            plugin_output(payload["plugin"].clone(), context)
        }
        "remove" => {
            let id = required(m, "id")?;
            let list = rpc(
                client,
                context,
                json!({"type":"plugin.list.request"}),
                "plugin.list.response",
            )
            .await?;
            let mut plugin = array(&list["plugins"])
                .iter()
                .find(|p| p["id"] == id)
                .cloned()
                .ok_or_else(|| anyhow!("Plugin is not configured: {id}"))?;
            rpc(
                client,
                context,
                json!({"type":"plugin.remove.request","pluginId":id}),
                "plugin.remove.response",
            )
            .await?;
            plugin["enabled"] = json!(false);
            plugin["status"] = json!("disabled");
            plugin_output(plugin, context)
        }
        _ => Err(anyhow!("Unknown plugin command: {command}")),
    }
}
fn portable(path: &str) -> bool {
    path == "."
        || (!path.is_empty()
            && !path.contains(':')
            && path
                .split(['/', '\\'])
                .all(|p| !p.is_empty() && p != "." && p != ".."))
}
fn source_reference(source: &str, path: Option<&str>) -> Result<String> {
    let (base, suffix) = crate::plugins::split_source_reference(source)?;
    let windows = base.as_bytes().get(1) == Some(&b':')
        && base
            .as_bytes()
            .get(2)
            .is_some_and(|b| *b == b'/' || *b == b'\\');
    let directory = base.starts_with('/')
        || base.starts_with('~')
        || base.starts_with("./")
        || base.starts_with("../")
        || base.starts_with(".\\")
        || base.starts_with("..\\")
        || base == "."
        || base == ".."
        || base.starts_with("\\\\")
        || windows;
    let mut source = if directory {
        let expanded = if base == "~" {
            dirs::home_dir()
                .ok_or_else(|| anyhow!("User home unavailable"))?
                .to_string_lossy()
                .into_owned()
        } else if let Some(rest) = base.strip_prefix("~/") {
            dirs::home_dir()
                .ok_or_else(|| anyhow!("User home unavailable"))?
                .join(rest)
                .to_string_lossy()
                .into_owned()
        } else {
            base
        };
        if Path::new(&expanded).is_absolute() || windows || expanded.starts_with("\\\\") {
            expanded
        } else {
            std::env::current_dir()?
                .join(expanded)
                .to_string_lossy()
                .into_owned()
        }
    } else {
        base
    };
    if let Some(suffix) = suffix {
        source.push(':');
        source.push_str(&suffix);
    }
    if let Some(path) = path {
        if !portable(path) {
            return Err(anyhow!(
                "Plugin path must be a portable path relative to the repository"
            ));
        }
        source.push(':');
        source.push_str(path);
    }
    Ok(source)
}
async fn update(m: &ArgMatches, context: &Context, client: &DaemonClient) -> Result<Output> {
    let id = string(m, "id");
    let all = flag(m, "all");
    if id.is_some() == all {
        return Err(anyhow!("Choose one plugin ID or pass --all"));
    }
    let reference = string(m, "ref");
    let version = string(m, "version");
    if reference.is_some() && version.is_some() {
        return Err(anyhow!("Choose --ref or --version, not both"));
    }
    let target = reference
        .map(|v| json!({"kind":"git","ref":v}))
        .or_else(|| version.map(|v| json!({"kind":"npm","version":v})));
    if target.is_some() && all {
        return Err(anyhow!(
            "An explicit target requires one plugin ID, without --all"
        ));
    }
    let mut request = json!({"type":"plugin.source.update.preview.request"});
    if let Some(id) = id {
        request["pluginId"] = json!(id);
    }
    if let Some(target) = &target {
        request["target"] = target.clone();
    }
    let preview = client
        .request_cancellable(
            request,
            "plugin.source.update.preview.response",
            Duration::from_secs(300),
            context.cancel.clone(),
        )
        .await?;
    let mut rows = response_array(&preview, "plugins")?.to_vec();
    if context.output.format == Format::Table {
        for row in &rows {
            let current = row["current"]["currentRevision"]
                .as_str()
                .unwrap_or("unknown");
            let target = row["target"]["commit"]
                .as_str()
                .or_else(|| row["target"]["version"].as_str())
                .unwrap_or("unknown");
            let detail = match text(row, "outcome") {
                "update" => format!("{current} → {target}"),
                "current" => format!("up to date ({current})"),
                "installed-newer" => format!(
                    "installed {current} is newer than latest {target}; keeping installed version"
                ),
                "local" => "local directory; use Reload after editing".into(),
                _ => row["error"].as_str().unwrap_or("Check failed").into(),
            };
            eprintln!("{}: {detail}", text(row, "id"));
            for link in array(&row["links"]) {
                if let Some(link) = link.as_str() {
                    eprintln!("Review: {link}");
                }
            }
        }
    }
    let proposals: Vec<Value> = rows
        .iter()
        .filter_map(|r| r.get("proposal").filter(|p| p.is_object()).cloned())
        .collect();
    if !flag(m, "check") && !proposals.is_empty() {
        if !flag(m, "yes") && target.is_none() {
            return Err(anyhow!("Confirmation required; rerun with --yes."));
        }
        let applied = client
            .request_cancellable(
                json!({"type":"plugin.source.update.apply.request","proposals":proposals}),
                "plugin.source.update.apply.response",
                Duration::from_secs(300),
                context.cancel.clone(),
            )
            .await?;
        let applied = response_array(&applied, "plugins")?;
        for row in &mut rows {
            if let Some(applied) = applied.iter().find(|a| a["id"] == row["id"]) {
                *row = applied.clone();
            }
        }
    }
    let code = if rows.iter().any(|r| r["outcome"] == "error") {
        1
    } else {
        0
    };
    if context.output.format != Format::Table || context.output.quiet {
        return Ok(Output::new(json!(rows)).exit(code));
    }
    let display:Vec<Value>=rows.iter().map(|r|json!({"id":r["id"],"outcome":r["outcome"],"detail":r["error"].as_str().or_else(||r["warning"].as_str()).or_else(||r["plugin"]["installation"]["currentRevision"].as_str()).unwrap_or("")})).collect();
    Ok(Output::new(json!(rows))
        .table_data(json!(display))
        .columns(&[
            ("PLUGIN", "id"),
            ("RESULT", "outcome"),
            ("DETAIL", "detail"),
        ])
        .widths(&[24, 18, 50])
        .exit(code))
}
pub(super) async fn init(m: &ArgMatches) -> Result<Output> {
    let path = PathBuf::from(required(m, "directory")?);
    let absolute = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut directory = PathBuf::new();
    for part in absolute.components() {
        match part {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                directory.pop();
            }
            _ => directory.push(part.as_os_str()),
        }
    }
    let id = string(m, "id")
        .or_else(|| directory.file_name().and_then(|v| v.to_str()))
        .ok_or_else(|| anyhow!("Plugin ID is required"))?;
    if id.is_empty()
        || !id.as_bytes()[0].is_ascii_lowercase()
        || !id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b))
    {
        return Err(anyhow!("Invalid plugin ID: {id}"));
    }
    tokio::fs::create_dir_all(&directory).await?;
    if tokio::fs::read_dir(&directory)
        .await?
        .next_entry()
        .await?
        .is_some()
    {
        return Err(anyhow!(
            "Plugin directory must be empty: {}",
            directory.display()
        ));
    }
    let version = env!("CARGO_PKG_VERSION");
    let manifest = json!({"id":id,"media":[],"requirements":{"paseo":format!(">={version}")}});
    let package = json!({"name":id,"private":true,"version":"0.0.0","files":["paseo-plugin.json","OVERVIEW.md","OVERVIEW.*.md","index.server.ts","server/","shared/"],"scripts":{"typecheck":"tsc --noEmit"},"devDependencies":{"@getpaseo/plugin":version,"typescript":"^5.9.3","zod":"^4.4.3"}});
    let tsconfig = json!({"compilerOptions":{"target":"ES2020","module":"ESNext","moduleResolution":"Bundler","lib":["ES2023"],"strict":true,"skipLibCheck":true,"noEmit":true,"esModuleInterop":true,"allowSyntheticDefaultImports":true},"include":["**/*.ts"]});
    let files=[("paseo-plugin.json",format!("{}\n",serde_json::to_string_pretty(&manifest)?)),("package.json",format!("{}\n",serde_json::to_string_pretty(&package)?)),("tsconfig.json",format!("{}\n",serde_json::to_string_pretty(&tsconfig)?)),("index.server.ts","import type { PluginServerContext } from \"@getpaseo/plugin/server\";\nimport { createGreeting } from \"./server/greeting\";\nimport { greetingRpc } from \"./shared/greeting\";\n\nexport default function contribute(server: PluginServerContext) {\n  server.handle(greetingRpc, createGreeting);\n  return () => {};\n}\n".into()),("shared/greeting.ts","import { defineRpc } from \"@getpaseo/plugin\";\nimport { z } from \"zod\";\n\nexport const greetingRpc = defineRpc({\n  name: \"greeting.create\",\n  input: z.object({ name: z.string() }),\n  output: z.object({ message: z.string() }),\n});\n".into()),("server/greeting.ts","import type { RpcInput } from \"@getpaseo/plugin\";\nimport { greetingRpc } from \"../shared/greeting\";\n\nexport function createGreeting({ name }: RpcInput<typeof greetingRpc>) {\n  return { message: \"Hello, \" + name + \"!\" };\n}\n".into())];
    for name in ["server", "shared"] {
        tokio::fs::create_dir(directory.join(name)).await?;
    }
    for (name, content) in files {
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(directory.join(name))
            .await?;
        file.write_all(content.as_bytes()).await?;
    }
    let mut overview = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(directory.join("OVERVIEW.md"))
        .await?;
    overview.write_all(OVERVIEW.as_bytes()).await?;
    Ok(Output::new(json!({"id":id,"directory":directory}))
        .columns(&[("PLUGIN", "id"), ("DIRECTORY", "directory")])
        .widths(&[20, 60]))
}
const OVERVIEW: &str = r#"<!--
Use this file to describe your backend plugin for its registry listing. Keep it
beside paseo-plugin.json at the pinned commit and in your npm package.
Replace this comment with useful facts before publishing.

This overview is for people choosing a plugin in Paseo, on the plugin page under
its install command. README assumes a GitHub audience and includes technical
details, installation instructions, and badges. Long, AI-generated READMEs leave
people reading past that material to understand what the plugin does.

Write these parts in order, including only what helps someone decide to install:

1. Describe what your plugin is and does in plain terms first.
2. Explain how it works only when it is not obvious.
3. Explain setup when needed: settings, accounts, tokens, providers, tools, or
   other plugins. Include applicable daemon version and operating system
   requirements. Setup guidance is allowed; installation instructions are not.
4. Explain capabilities and settings worth understanding, what each option does,
   what the plugin reads or sends and where, permissions, and known limits.

Length follows complexity; a simple backend utility may need one paragraph.
Choose headings only when they help. Use plain factual language.
Omit installation commands, badges, changelog, contributing or license sections,
marketing, and unsupported claims. Avoid implementation filler such as empty
cleanup functions or lists of absent features.

To translate the overview, add a whole copy beside this file named for the
language, such as OVERVIEW.zh-CN.md. Readers in that language see it instead.
-->
"#;
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn source_paths_are_portable() {
        assert!(portable("."));
        assert!(portable("plugins/test"));
        for path in [
            "/absolute",
            "../escape",
            "nested/../escape",
            "C:\\root",
            "a//b",
        ] {
            assert!(!portable(path));
        }
    }
    #[test]
    fn windows_install_paths_are_not_made_local() {
        assert_eq!(
            source_reference("C:\\plugins\\hello", None).unwrap(),
            "C:\\plugins\\hello"
        );
    }
    #[test]
    fn managed_identity_has_explicit_source() {
        assert_eq!(
            identity(&json!({"kind":"npm","packageName":"@scope/plugin","pluginPath":"."})),
            "npm:@scope/plugin"
        );
    }
    #[tokio::test]
    async fn initialization_never_overwrites_existing_content() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("my-plugin");
        tokio::fs::create_dir(&directory).await.unwrap();
        tokio::fs::write(directory.join("keep"), b"user content")
            .await
            .unwrap();
        let matches = command()
            .try_get_matches_from([
                "plugin".to_owned(),
                "init".into(),
                directory.to_string_lossy().into_owned(),
            ])
            .unwrap();
        assert!(
            init(matches.subcommand_matches("init").unwrap())
                .await
                .err()
                .unwrap()
                .to_string()
                .contains("must be empty")
        );
        assert_eq!(
            tokio::fs::read(directory.join("keep")).await.unwrap(),
            b"user content"
        );
        assert!(!directory.join("paseo-plugin.json").exists());
    }
}
