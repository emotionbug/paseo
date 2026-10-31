use super::input_error;
use crate::state::AppState;
use anyhow::Result;
use chrono::{Duration, SecondsFormat, Utc};
use serde_json::{Value, json};
use std::sync::Arc;

fn definition(name: &str, description: &str, properties: Value, required: &[&str]) -> Value {
    let mut title = name.replace('_', " ");
    if let Some(first) = title.get_mut(..1) {
        first.make_ascii_uppercase();
    }
    json!({"name":name,"title":title,"description":description,"inputSchema":{"type":"object","properties":properties,"required":required,"additionalProperties":true}})
}
pub(crate) fn definitions(agent_scope: bool) -> Vec<Value> {
    let mut create = json!({"prompt":{"type":"string","minLength":1},"cron":{"type":"string","minLength":1},"timezone":{"type":"string","minLength":1,"description":"IANA time zone for the cron cadence. For example: America/New_York."},"name":{"type":"string"},"maxRuns":{"type":"integer","minimum":1},"expiresIn":{"type":"string"}});
    let heartbeat = create.clone();
    create["provider"] = json!({"type":"string","description":"Provider, or provider/model (for example: codex or codex/gpt-5.4). Defaults to the caller's provider in an agent-scoped session."});
    create["cwd"] = json!({"type":"string"});
    create["isolation"] = json!({"enum":["local","worktree"]});
    let mut out = vec![
        definition(
            "create_schedule",
            "Create a recurring schedule that starts a new agent on a cron cadence.",
            create,
            if agent_scope {
                &["prompt", "cron"]
            } else {
                &["prompt", "cron", "provider"]
            },
        ),
        definition(
            "create_heartbeat",
            "Create a recurring heartbeat that sends you a prompt on a cron cadence.",
            heartbeat,
            &["prompt", "cron"],
        ),
        definition(
            "list_schedules",
            "List all schedules managed by the daemon.",
            json!({}),
            &[],
        ),
        definition(
            "update_schedule",
            "Update an existing schedule. Only provided fields are changed; omitted fields remain unchanged.",
            json!({
                "id":{"type":"string"},
                "cron":{"type":"string","description":"New cron expression."},
                "timezone":{"type":"string","minLength":1,"description":"IANA time zone for cron cadence; requires cron. For example: America/New_York."},
                "name":{"type":["string","null"],"description":"New name (null to clear)."},
                "prompt":{"type":"string","minLength":1,"description":"New prompt text."},
                "maxRuns":{"type":["integer","null"],"minimum":1,"description":"New max runs limit (null to clear)."},
                "provider":{"type":"string","minLength":1,"description":"New provider for new-agent target."},
                "model":{"type":["string","null"],"minLength":1,"description":"New model for new-agent target (null to clear)."},
                "mode":{"type":["string","null"],"minLength":1,"description":"New mode for new-agent target (null to clear)."},
                "cwd":{"type":"string","minLength":1,"description":"New cwd for new-agent target."},
                "expiresIn":{"type":"string","description":"New relative expiry duration (for example: 1h, 2d)."},
                "clearExpires":{"type":"boolean","description":"Clear any schedule expiry."}
            }),
            &["id"],
        ),
    ];
    for (name, description) in [
        ("delete_heartbeat", "Delete one of your heartbeats."),
        (
            "inspect_schedule",
            "Inspect a schedule and its run history.",
        ),
        ("pause_schedule", "Pause an active schedule."),
        ("resume_schedule", "Resume a paused schedule."),
        ("delete_schedule", "Delete a schedule permanently."),
        (
            "schedule_logs",
            "Get the run history (logs) for a schedule.",
        ),
        (
            "run_schedule_once",
            "Run a schedule immediately without changing its cron cadence.",
        ),
    ] {
        let schema = if matches!(name, "delete_heartbeat" | "run_schedule_once") {
            json!({"id":{"type":"string","minLength":1}})
        } else {
            json!({"id":{"type":"string"}})
        };
        out.push(definition(name, description, schema, &["id"]));
    }
    for tool in &mut out {
        let name = tool["name"].as_str().unwrap_or("").to_owned();
        let fields: &[&str] = if name == "update_schedule" {
            &["timezone", "prompt", "provider", "model", "mode", "cwd"]
        } else if matches!(name.as_str(), "create_schedule" | "create_heartbeat") {
            &["prompt", "cron", "timezone"]
        } else {
            &[]
        };
        for key in fields {
            tool["inputSchema"]["properties"][*key]["x-trim"] = json!(true);
        }
        if matches!(name.as_str(), "create_schedule" | "create_heartbeat") {
            tool["inputSchema"]["x-strip-unknown"] = json!(true);
            tool["inputSchema"]["additionalProperties"] = json!(false);
        }
    }
    out
}
fn text<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}
fn trimmed<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    text(v, key).map(str::trim).filter(|s| !s.is_empty())
}
/// JavaScript truthiness, used where the source copies caller settings with `x ? {...} : {}`.
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|n| n != 0.0 && !n.is_nan()),
        Value::String(s) => !s.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}
fn provider_model(raw: &str) -> Result<(&str, Option<&str>)> {
    let raw = raw.trim();
    if let Some((p, m)) = raw.split_once('/') {
        if p.trim().is_empty() || m.trim().is_empty() {
            return Err(input_error(
                "provider must be <provider> or <provider>/<model>",
            ));
        }
        Ok((p.trim(), Some(m.trim())))
    } else {
        Ok((raw, None))
    }
}
fn duration_ms(raw: &str) -> Result<i64> {
    let s = raw.trim();
    if !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) {
        return s
            .parse::<i64>()
            .ok()
            .and_then(|v| v.checked_mul(1000))
            .ok_or_else(|| input_error("Invalid duration"));
    }
    let re = regex::Regex::new(r"(\d+)([smh])").expect("constant duration expression");
    let mut total = 0i64;
    let mut matched = false;
    for cap in re.captures_iter(s) {
        matched = true;
        let n = cap[1]
            .parse::<i64>()
            .map_err(|_| input_error("Invalid duration"))?;
        let factor = match &cap[2] {
            "s" => 1000,
            "m" => 60000,
            _ => 3600000,
        };
        total = total
            .checked_add(
                n.checked_mul(factor)
                    .ok_or_else(|| input_error("Invalid duration"))?,
            )
            .ok_or_else(|| input_error("Invalid duration"))?;
    }
    if !matched {
        return Err(input_error(format!(
            "Invalid duration format: {raw}. Use formats like: 5m, 30s, 1h, 2h30m"
        )));
    }
    Ok(total)
}
fn expires(raw: &str) -> Result<Value> {
    let date = Utc::now()
        .checked_add_signed(Duration::milliseconds(duration_ms(raw)?))
        .ok_or_else(|| input_error("Invalid duration"))?;
    Ok(json!(date.to_rfc3339_opts(SecondsFormat::Millis, true)))
}
fn cadence(input: &Value) -> Result<Option<Value>> {
    let cron = trimmed(input, "cron");
    let every = trimmed(input, "every");
    let timezone = trimmed(input, "timezone");
    if cron.is_some() && every.is_some() {
        return Err(input_error("Specify at most one of every or cron"));
    }
    if timezone.is_some() && cron.is_none() {
        return Err(input_error("timezone can only be used with cron"));
    }
    let expression = if let Some(c) = cron {
        Some(c.to_owned())
    } else if let Some(e) = every {
        let ms = duration_ms(e)?;
        let expr = if ms > 0 && ms % 60000 == 0 && ms <= 3600000 && 3600000 % ms == 0 {
            Some(if ms == 3600000 {
                "0 * * * *".into()
            } else {
                format!("*/{} * * * *", ms / 60000)
            })
        } else if ms > 0 && ms % 3600000 == 0 && ms <= 86400000 && 86400000 % ms == 0 {
            Some(if ms == 86400000 {
                "0 0 * * *".into()
            } else {
                format!("0 */{} * * *", ms / 3600000)
            })
        } else {
            None
        };
        Some(expr.ok_or_else(|| {
            input_error(format!(
                "{e} cannot be represented faithfully by five-field cron"
            ))
        })?)
    } else {
        None
    };
    let cadence = expression.map(|e| {
        let mut c = json!({"type":"cron","expression":e});
        if let Some(t) = timezone {
            c["timezone"] = json!(t);
        }
        c
    });
    if let Some(value) = &cadence {
        crate::automation::validate_cadence(value)
            .and_then(|_| crate::automation::next_run_at(value, Utc::now()).map(|_| ()))
            .map_err(|error| {
                let message = error.to_string();
                let path = if message.starts_with("Invalid cron time zone:") {
                    "timezone"
                } else {
                    "cron"
                };
                crate::providers::api::NativeToolInputError {
                    status: 400,
                    issues: Some(json!([{"code":"custom","path":[path],"message":message}])),
                    message,
                }
            })?;
    }
    Ok(cadence)
}
fn summary(mut value: Value) -> Value {
    if let Some(obj) = value.as_object_mut() {
        obj.remove("runs");
    }
    value
}
fn result(value: Value) -> Value {
    json!({"content":[],"structuredContent":value})
}
fn schedule_error(error: anyhow::Error) -> anyhow::Error {
    if crate::automation::is_input_error(&error) {
        input_error(error.to_string())
    } else {
        error
    }
}
async fn require_target(app: &Arc<AppState>, id: &str, kind: &str) -> Result<Value> {
    let record = app.automation.inspect(id).await.map_err(schedule_error)?;
    if record.pointer("/target/type").and_then(Value::as_str) != Some(kind) {
        return Err(input_error(format!(
            "{} not found: {id}",
            if kind == "agent" {
                "Heartbeat"
            } else {
                "Schedule"
            }
        )));
    }
    Ok(record)
}
pub(crate) async fn execute(
    app: &Arc<AppState>,
    caller: Option<&str>,
    name: &str,
    input: Value,
) -> Result<Value> {
    if name == "list_schedules" {
        let schedules = app
            .automation
            .list()
            .await?
            .into_iter()
            .filter(|r| r.pointer("/target/type").and_then(Value::as_str) == Some("new-agent"))
            .map(summary)
            .collect::<Vec<_>>();
        return Ok(result(json!({"schedules":schedules})));
    }
    if matches!(name, "create_schedule" | "create_heartbeat") {
        let cadence = cadence(&input)?.ok_or_else(|| input_error("cron is required"))?;
        let mut create =
            json!({"prompt":text(&input,"prompt").unwrap_or("").trim(),"cadence":cadence});
        if create["prompt"] == "" {
            return Err(input_error("prompt is required"));
        }
        let parent = if let Some(id) = caller {
            Some(
                app.agents
                    .agent_snapshot(id)
                    .await?
                    .ok_or_else(|| input_error(format!("Parent agent {id} not found")))?,
            )
        } else {
            None
        };
        create["target"] = if name == "create_heartbeat" {
            json!({"type":"agent","agentId":caller.ok_or_else(||input_error("create_heartbeat requires an agent-scoped session"))?})
        } else {
            let parent_config = parent
                .as_ref()
                .and_then(|p| p.get("config"))
                .unwrap_or(&Value::Null);
            let parent_provider = parent
                .as_ref()
                .and_then(|p| text(p, "provider"))
                .or_else(|| text(parent_config, "provider"));
            let provider_input = trimmed(&input, "provider")
                .or(parent_provider)
                .ok_or_else(|| input_error("provider is required when target is new-agent"))?;
            let (provider, model) = provider_model(provider_input)?;
            // expandUserPath: tilde expansion plus path.resolve against the daemon cwd.
            let cwd = if let Some(c) = trimmed(&input, "cwd") {
                super::workspace_tools::expand_path(app, c, None)?
            } else if let Some(id) = caller {
                app.agents.cwd(id).await?
            } else {
                std::env::current_dir()?
            };
            let mut config = json!({"provider":provider,"cwd":cwd});
            if let Some(m) = model {
                config["model"] = json!(m);
            } else if input.get("provider").is_none() {
                if let Some(m) = parent_config.get("model").filter(|v| truthy(v)) {
                    config["model"] = m.clone();
                }
            }
            if parent_provider == Some(provider) {
                if let Some(mode) = parent
                    .as_ref()
                    .and_then(|p| p.get("currentModeId"))
                    .filter(|v| truthy(v))
                {
                    config["modeId"] = mode.clone();
                }
                if let Some(v) = parent_config.get("providerOptions").filter(|v| truthy(v)) {
                    config["providerOptions"] = v.clone();
                }
            }
            for key in [
                "thinkingOptionId",
                "featureValues",
                "systemPrompt",
                "mcpServers",
            ] {
                if let Some(v) = parent_config.get(key).filter(|v| truthy(v)) {
                    config[key] = v.clone();
                }
            }
            if let Some(v) = input.get("isolation") {
                config["isolation"] = v.clone();
            }
            json!({"type":"new-agent","config":config})
        };
        if let Some(n) = trimmed(&input, "name") {
            create["name"] = json!(n);
        }
        if let Some(v) = input.get("maxRuns") {
            create["maxRuns"] = v.clone();
        }
        if let Some(v) = text(&input, "expiresIn") {
            create["expiresAt"] = expires(v)?;
        }
        return Ok(result(summary(
            app.automation.create_or_replace(&create).await?,
        )));
    }
    let id = text(&input, "id").ok_or_else(|| input_error("id is required"))?;
    if name == "delete_heartbeat" {
        let caller = caller
            .ok_or_else(|| input_error("Heartbeat operations require an agent-scoped session"))?;
        let record = require_target(app, id, "agent").await?;
        if record.pointer("/target/agentId").and_then(Value::as_str) != Some(caller) {
            return Err(input_error(format!(
                "Heartbeat {id} does not belong to caller {caller}"
            )));
        }
        app.automation.delete(id).await?;
        return Ok(result(json!({"success":true})));
    }
    let existing = require_target(app, id, "new-agent").await?;
    let value = match name {
        "inspect_schedule" => existing,
        "schedule_logs" => json!({"runs":app.automation.logs(id).await?}),
        "pause_schedule" => {
            app.automation.pause(id).await.map_err(schedule_error)?;
            json!({"success":true})
        }
        "resume_schedule" => {
            app.automation.resume(id).await.map_err(schedule_error)?;
            json!({"success":true})
        }
        "delete_schedule" => {
            app.automation.delete(id).await?;
            json!({"success":true})
        }
        "run_schedule_once" => app.automation.run_once(id).await.map_err(schedule_error)?,
        "update_schedule" => {
            let mut patch = json!({"id":id});
            for key in ["name", "prompt", "maxRuns"] {
                if let Some(v) = input.get(key) {
                    patch[key] = v.clone();
                }
            }
            if let Some(c) = cadence(&input)? {
                patch["cadence"] = c;
            }
            if input.get("expiresIn").is_some() && input["clearExpires"] == true {
                return Err(input_error(
                    "Specify at most one of expiresIn or clearExpires",
                ));
            }
            if let Some(v) = text(&input, "expiresIn") {
                patch["expiresAt"] = expires(v)?;
            } else if input["clearExpires"] == true {
                patch["expiresAt"] = Value::Null;
            }
            let mut config = json!({});
            if let Some(p) = trimmed(&input, "provider") {
                let (p, m) = provider_model(p)?;
                config["provider"] = json!(p);
                if let Some(m) = m {
                    if input.get("model") == Some(&Value::Null) {
                        return Err(input_error("provider specifies a model but model is null"));
                    }
                    if let Some(other) = text(&input, "model") {
                        if other.trim() != m {
                            return Err(input_error("Conflicting model values provided"));
                        }
                    }
                    config["model"] = json!(m);
                }
            }
            for (from, to) in [("model", "model"), ("mode", "modeId"), ("cwd", "cwd")] {
                if let Some(v) = input.get(from) {
                    config[to] = v.clone();
                }
            }
            if config.as_object().is_some_and(|v| !v.is_empty()) {
                patch["newAgentConfig"] = config;
            }
            app.automation.update(&patch).await?
        }
        _ => return Err(input_error(format!("Paseo tool not found: {name}"))),
    };
    Ok(result(value))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn update_cadence_rejects_inconsistent_selectors() {
        assert!(cadence(&json!({"cron":"* * * * *","every":"1m"})).is_err());
        assert!(cadence(&json!({"timezone":"UTC"})).is_err());
    }
    #[test]
    fn provider_model_retains_model_slashes() {
        assert_eq!(
            provider_model("openrouter/vendor/model").unwrap(),
            ("openrouter", Some("vendor/model"))
        );
    }
    #[test]
    fn caller_settings_inherit_only_when_set() {
        for empty in [json!(null), json!(""), json!(false), json!(0)] {
            assert!(!truthy(&empty), "{empty}");
        }
        for set in [json!("gpt-5.4"), json!({}), json!([]), json!(true)] {
            assert!(truthy(&set), "{set}");
        }
    }
}
