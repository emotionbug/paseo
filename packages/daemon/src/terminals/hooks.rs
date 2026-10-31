use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::Write,
    path::{Path, PathBuf},
    time::Duration,
};

const CLAUDE_EVENTS: &[&str] = &[
    "UserPromptSubmit",
    "Stop",
    "StopFailure",
    "SessionEnd",
    "Notification",
];
const CODEX_EVENTS: &[&str] = &[
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "PermissionRequest",
    "Stop",
];
const OPENCODE_PLUGIN: &str = include_str!("opencode-activity.js");

pub fn resolve_activity(provider: &str, event: &str, input: Option<&str>) -> Option<&'static str> {
    match provider.to_ascii_lowercase().as_str() {
        "claude" => match event {
            "UserPromptSubmit" => Some("running"),
            "Stop" | "StopFailure" | "SessionEnd" => Some("idle"),
            "Notification" => {
                let value: Value = serde_json::from_str(input?).ok()?;
                if value["notification_type"] == "idle_prompt" {
                    Some("needs-input")
                } else {
                    None
                }
            }
            _ => None,
        },
        "codex" => match event {
            "UserPromptSubmit" | "PreToolUse" | "PostToolUse" => Some("running"),
            "PermissionRequest" => Some("needs-input"),
            "Stop" => Some("idle"),
            _ => None,
        },
        "opencode" => match event {
            "session.status.busy" | "session.status.retry" | "permission.replied" => {
                Some("running")
            }
            "session.status.idle" => Some("idle"),
            "permission.asked" => Some("needs-input"),
            _ => None,
        },
        _ => None,
    }
}

/// Hook delivery is best effort in the source CLI: activity reporting must never
/// fail an agent tool or prompt. This does not retry ambiguously delivered POSTs.
pub async fn post_activity(state: &str, env: &BTreeMap<String, String>) -> Result<()> {
    let Some(id) = env
        .get("PASEO_TERMINAL_ID")
        .filter(|value| !value.is_empty())
    else {
        return Ok(());
    };
    let Some(token) = env
        .get("PASEO_ACTIVITY_TOKEN")
        .filter(|value| !value.is_empty())
    else {
        return Ok(());
    };
    let Some(url) = env
        .get("PASEO_TERMINAL_ACTIVITY_URL")
        .filter(|value| !value.is_empty())
    else {
        return Ok(());
    };
    let client = reqwest::Client::builder()
        .timeout(Duration::from_millis(500))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let _ = client
        .post(url)
        .json(&json!({"terminalId":id,"token":token,"state":state}))
        .send()
        .await;
    Ok(())
}

pub(super) fn apply(enabled: bool, home: &Path, env: &BTreeMap<String, String>) -> Result<()> {
    for provider in ["claude", "codex", "opencode"] {
        if let Err(error) = update(provider, enabled, home, env) {
            if enabled {
                tracing::warn!(%error,provider,"Failed to install terminal activity hook provider");
            } else {
                return Err(error);
            }
        }
    }
    Ok(())
}

fn config_path(provider: &str, home: &Path, env: &BTreeMap<String, String>) -> Result<PathBuf> {
    let (path, file) = match provider {
        "claude" => (
            env.get("CLAUDE_CONFIG_DIR")
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".claude")),
            "settings.json",
        ),
        "codex" => (
            env.get("CODEX_HOME")
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".codex")),
            "hooks.json",
        ),
        "opencode" => {
            let dir = env
                .get("OPENCODE_CONFIG_DIR")
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    env.get("XDG_CONFIG_HOME")
                        .filter(|value| !value.is_empty())
                        .map(PathBuf::from)
                        .unwrap_or_else(|| home.join(".config"))
                        .join("opencode")
                });
            (dir, "plugins/paseo-terminal-activity.js")
        }
        _ => bail!("Unknown terminal hook provider"),
    };
    Ok(path.join(file))
}
fn read_optional(path: &Path) -> Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(raw) => Ok(Some(raw)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}
fn update(
    provider: &str,
    enabled: bool,
    home: &Path,
    env: &BTreeMap<String, String>,
) -> Result<()> {
    let path = config_path(provider, home, env)?;
    let current = read_optional(&path)?;
    if provider == "opencode" {
        if !enabled {
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            return Ok(());
        }
        if current.as_deref().map(normalize).as_deref() != Some(OPENCODE_PLUGIN) {
            write_private(&path, OPENCODE_PLUGIN)?;
        }
        return Ok(());
    }
    let mut value = match current.as_deref() {
        Some(raw) => serde_json::from_str::<Value>(raw)?,
        None => json!({}),
    };
    if !value.is_object() {
        value = json!({});
    }
    merge_hooks(&mut value, provider, enabled);
    let next = format!("{}\n", serde_json::to_string_pretty(&value)?);
    if current.as_deref().map(normalize).as_deref() != Some(next.as_str()) {
        write_private(&path, &next)?;
    }
    Ok(())
}
fn normalize(raw: &str) -> String {
    if raw.ends_with('\n') {
        raw.to_owned()
    } else {
        format!("{raw}\n")
    }
}
fn marker_matches(hook: &Value, provider: &str) -> bool {
    let marker = format!("hooks {provider}");
    let matches = |key: &str| {
        hook[key]
            .as_str()
            .is_some_and(|command| command.contains(&marker))
    };
    matches("command")
        || (provider == "codex" && (matches("commandWindows") || matches("command_windows")))
}
fn merge_hooks(value: &mut Value, provider: &str, enabled: bool) {
    let events = if provider == "claude" {
        CLAUDE_EVENTS
    } else {
        CODEX_EVENTS
    };
    if !value["hooks"].is_object() {
        value["hooks"] = json!({});
    }
    let hooks = value["hooks"].as_object_mut().expect("normalized hook map");
    for event in events {
        let mut entries = Vec::new();
        if let Some(groups) = hooks.get(*event).and_then(Value::as_array) {
            for group in groups.iter().filter(|group| group.is_object()) {
                let remaining: Vec<_> = group["hooks"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|hook| hook.is_object() && !marker_matches(hook, provider))
                    .cloned()
                    .collect();
                if !remaining.is_empty() {
                    let mut group = group.clone();
                    group["hooks"] = json!(remaining);
                    entries.push(group);
                }
            }
        }
        if enabled {
            let command = format!(
                "if [ -n \"$PASEO_TERMINAL_ID\" ]; then \"${{PASEO_HOOK_CLI:-paseo}}\" hooks {provider} {event}; fi"
            );
            let mut hook = json!({"type":"command","command":command,"timeout":10});
            if provider == "codex" {
                hook["commandWindows"] = json!(format!(
                    "if defined PASEO_TERMINAL_ID (if defined PASEO_HOOK_CLI (\"%PASEO_HOOK_CLI%\" hooks {provider} {event}) else (paseo hooks {provider} {event})) else (exit /b 0)"
                ));
            }
            entries.push(json!({"matcher":"","hooks":[hook]}));
        }
        if entries.is_empty() {
            hooks.remove(*event);
        } else {
            hooks.insert((*event).into(), json!(entries));
        }
    }
}
fn write_private(path: &Path, content: &str) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("Hook path has no parent"))?;
    std::fs::create_dir_all(parent)?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temp.as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    temp.write_all(content.as_bytes())?;
    temp.as_file().sync_all()?;
    temp.persist(path).map_err(|error| error.error)?;
    #[cfg(unix)]
    std::fs::File::open(parent)?.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn notification_payload_is_not_guessed() {
        assert_eq!(
            resolve_activity(
                "CLAUDE",
                "Notification",
                Some(r#"{"notification_type":"idle_prompt"}"#)
            ),
            Some("needs-input")
        );
        assert_eq!(
            resolve_activity(
                "claude",
                "Notification",
                Some(r#"{"notification_type":"permission_prompt"}"#)
            ),
            None
        );
        assert_eq!(resolve_activity("claude", "Notification", None), None);
        assert_eq!(
            resolve_activity("codex", "PermissionRequest", None),
            Some("needs-input")
        );
    }
    #[test]
    fn installing_and_removing_preserves_foreign_hooks_and_metadata() {
        let mut value = json!({"model":"kept","hooks":{"Stop":[{"matcher":"custom","unknown":42,"hooks":[{"type":"command","command":"keep"},{"command_windows":"paseo hooks codex Stop"}]}],"Foreign":[{"hooks":[{"command":"other"}]}]}});
        merge_hooks(&mut value, "codex", true);
        assert_eq!(value["hooks"]["Stop"].as_array().unwrap().len(), 2);
        assert_eq!(value["hooks"]["Stop"][0]["unknown"], 42);
        merge_hooks(&mut value, "codex", false);
        assert_eq!(value["hooks"]["Stop"].as_array().unwrap().len(), 1);
        assert_eq!(value["hooks"]["Stop"][0]["hooks"][0]["command"], "keep");
        assert_eq!(value["model"], "kept");
        assert!(value["hooks"].get("Foreign").is_some());
    }
    #[test]
    fn isolated_install_is_idempotent_and_path_overrides_are_preserved() {
        let home = tempfile::tempdir().unwrap();
        let env = BTreeMap::new();
        apply(true, home.path(), &env).unwrap();
        let first = std::fs::read(home.path().join(".claude/settings.json")).unwrap();
        apply(true, home.path(), &env).unwrap();
        assert_eq!(
            first,
            std::fs::read(home.path().join(".claude/settings.json")).unwrap()
        );
        apply(false, home.path(), &env).unwrap();
        assert!(
            !home
                .path()
                .join(".config/opencode/plugins/paseo-terminal-activity.js")
                .exists()
        );
    }
}
