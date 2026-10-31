mod daemon;
mod hosts;
mod hub;
mod prompts;

use super::{Context, option, output::Output, switch};
use anyhow::{Result, ensure};
use clap::{ArgMatches, Command};
use serde_json::Value;

pub fn commands() -> Vec<Command> {
    vec![
        daemon::command(),
        hosts::command(),
        hub::command(),
        // The original CLI offers config reload both here and as `daemon reload`.
        Command::new("reload").about("Reload config.json without restarting"),
        Command::new("onboard")
            .about("Configure and start the local daemon without terminal prompts")
            .arg(option("listen", "listen").conflicts_with("port"))
            .arg(option("port", "port"))
            .arg(switch("relay", "relay").conflicts_with("no-relay"))
            .arg(switch("no-relay", "no-relay"))
            .arg(switch("no-mcp", "no-mcp"))
            .arg(option("hostnames", "hostnames").alias("allowed-hosts"))
            .arg(option("timeout", "timeout"))
            .arg(option("voice", "voice").value_parser(["ask", "enable", "disable"])),
    ]
}
pub async fn execute(
    path: &[&str],
    matches: &ArgMatches,
    context: &Context,
) -> Result<Option<Output>> {
    let result = match path.first().copied() {
        Some("daemon") => daemon::execute(&path[1..], matches, context).await?,
        Some("host") => hosts::execute(&path[1..], matches, context).await?,
        Some("hub") => hub::execute(&path[1..], matches, context).await?,
        Some("reload") => daemon::execute(&["reload"], matches, context).await?,
        Some("onboard") => daemon::onboard(matches, context).await?,
        _ => return Ok(None),
    };
    Ok(Some(result))
}
pub(crate) fn error(code: &str, message: impl Into<String>) -> anyhow::Error {
    crate::client::ClientError::new(code, message).into()
}
pub(crate) fn duration(
    matches: &ArgMatches,
    key: &str,
    default: f64,
) -> Result<std::time::Duration> {
    let invalid = || {
        error(
            "INVALID_TIMEOUT",
            "Timeout must be a positive number of seconds.",
        )
    };
    let seconds = if let Some(raw) = super::string(matches, key) {
        let raw = raw.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}');
        if let Some(digits) = raw.strip_prefix("0x").or_else(|| raw.strip_prefix("0X")) {
            u64::from_str_radix(digits, 16).map_err(|_| invalid())? as f64
        } else if let Some(digits) = raw.strip_prefix("0b").or_else(|| raw.strip_prefix("0B")) {
            u64::from_str_radix(digits, 2).map_err(|_| invalid())? as f64
        } else if let Some(digits) = raw.strip_prefix("0o").or_else(|| raw.strip_prefix("0O")) {
            u64::from_str_radix(digits, 8).map_err(|_| invalid())? as f64
        } else {
            raw.parse::<f64>().map_err(|_| invalid())?
        }
    } else {
        default
    };
    ensure!(seconds.is_finite() && seconds > 0.0, invalid());
    std::time::Duration::try_from_secs_f64((seconds * 1000.0).ceil() / 1000.0)
        .map_err(|_| invalid())
}
pub(crate) fn pretty(value: Value) -> Result<Output> {
    let human = serde_json::to_string_pretty(&value)?;
    Ok(Output::new(value).human(human))
}
pub(crate) fn set_path(value: &mut Value, path: &str, item: Option<Value>) -> Result<()> {
    let parts: Vec<_> = path.split('.').collect();
    ensure!(
        !parts
            .iter()
            .any(|part| part.is_empty()
                || matches!(*part, "__proto__" | "prototype" | "constructor")),
        "Invalid configuration path"
    );
    let mut cursor = value;
    for part in &parts[..parts.len() - 1] {
        if item.is_none() && cursor.get(*part).is_none() {
            return Ok(());
        }
        if cursor.is_null() {
            *cursor = serde_json::json!({});
        }
        let object = cursor.as_object_mut().ok_or_else(|| {
            error(
                "CONFIG_PATH_INVALID",
                "Configuration path crosses a non-object",
            )
        })?;
        cursor = object
            .entry((*part).to_owned())
            .or_insert_with(|| serde_json::json!({}));
    }
    if item.is_none() && cursor.is_null() {
        return Ok(());
    }
    if cursor.is_null() {
        *cursor = serde_json::json!({});
    }
    let object = cursor.as_object_mut().ok_or_else(|| {
        error(
            "CONFIG_PATH_INVALID",
            "Configuration parent is not an object",
        )
    })?;
    let key = parts[parts.len() - 1];
    if let Some(item) = item {
        object.insert(key.into(), item);
    } else {
        object.remove(key);
    }
    Ok(())
}
pub(crate) use daemon::{observe_status, restart_confirmed};
