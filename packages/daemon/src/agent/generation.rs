use std::{fmt, path::Path, sync::LazyLock};

use anyhow::{Result, anyhow};
use regex::Regex;
use serde::Serialize;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::AgentManager;

#[derive(Debug, thiserror::Error)]
#[error("Agent response did not match the required JSON schema")]
pub struct StructuredAgentResponseError {
    pub last_response: String,
    pub validation_errors: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StructuredGenerationAttempt {
    pub provider: String,
    pub model: Option<String>,
    pub available: bool,
    pub error: Option<String>,
}

#[derive(Debug)]
pub struct StructuredAgentFallbackError {
    pub attempts: Vec<StructuredGenerationAttempt>,
}

impl fmt::Display for StructuredAgentFallbackError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Structured generation failed for all providers")?;
        for (index, attempt) in self.attempts.iter().enumerate() {
            f.write_str(if index == 0 { ": " } else { "; " })?;
            f.write_str(&attempt.provider)?;
            if let Some(model) = &attempt.model {
                write!(f, " ({model})")?;
            }
            f.write_str(if attempt.available {
                ": failed"
            } else {
                ": unavailable"
            })?;
            if let Some(error) = &attempt.error {
                write!(f, " ({error})")?;
            }
        }
        Ok(())
    }
}

impl std::error::Error for StructuredAgentFallbackError {}

#[derive(Debug, thiserror::Error)]
#[error("Structured generation cancelled")]
pub struct StructuredGenerationCancelled;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Candidate {
    provider: String,
    model: Option<String>,
    thinking_option_id: Option<String>,
}

fn text<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
}

fn models(entry: &Value) -> &[Value] {
    entry
        .get("models")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
}

fn thinking(model: Option<&Value>, preferred: Option<&str>) -> Option<String> {
    let model = model?;
    if let Some(preferred) = preferred {
        if model["thinkingOptions"].as_array().is_some_and(|options| {
            options
                .iter()
                .any(|option| option["id"].as_str() == Some(preferred))
        }) {
            return Some(preferred.to_owned());
        }
    }
    text(model, "defaultThinkingOptionId").map(str::to_owned)
}

fn configured_candidate(input: &Value, enabled: &[&Value]) -> Option<Candidate> {
    let provider = text(input, "provider")?.trim();
    if provider.is_empty() {
        return None;
    }
    let model = text(input, "model")
        .map(str::trim)
        .filter(|model| !model.is_empty());
    let preferred = text(input, "thinkingOptionId").filter(|option| !option.is_empty());
    if let Some(entry) = enabled
        .iter()
        .find(|entry| entry["provider"].as_str() == Some(provider))
    {
        if let Some(model) = model {
            return Some(Candidate {
                provider: provider.to_owned(),
                model: Some(model.to_owned()),
                thinking_option_id: preferred.map(str::to_owned),
            });
        }
        let default = models(entry)
            .iter()
            .find(|model| model["isDefault"] == true)
            .or_else(|| models(entry).first());
        return Some(Candidate {
            provider: provider.to_owned(),
            model: default
                .and_then(|model| text(model, "id"))
                .map(str::to_owned),
            thinking_option_id: thinking(default, preferred),
        });
    }
    if let Some(model_id) = model {
        let normalized_provider = provider.to_lowercase();
        let normalized_model = model_id.to_lowercase();
        let combined = format!("{normalized_provider}/{normalized_model}");
        for entry in enabled {
            for model in models(entry) {
                let metadata = &model["metadata"];
                if !text(metadata, "providerId").is_some_and(|value| {
                    !value.trim().is_empty() && value.to_lowercase() == normalized_provider
                }) {
                    continue;
                }
                let Some(id) = text(model, "id") else {
                    continue;
                };
                let normalized_id = id.to_lowercase();
                if normalized_id == normalized_model
                    || normalized_id == combined
                    || text(metadata, "modelId").is_some_and(|value| {
                        !value.trim().is_empty() && value.to_lowercase() == normalized_model
                    })
                {
                    return Some(Candidate {
                        provider: entry["provider"].as_str()?.to_owned(),
                        model: Some(id.to_owned()),
                        thinking_option_id: thinking(Some(model), preferred),
                    });
                }
            }
        }
    }
    Some(Candidate {
        provider: provider.to_owned(),
        model: model.map(str::to_owned),
        thinking_option_id: preferred.map(str::to_owned),
    })
}

fn resolve_candidates(
    entries: &[Value],
    config: &Value,
    selection: Option<&Value>,
) -> Vec<Candidate> {
    let enabled: Vec<_> = entries
        .iter()
        .filter(|entry| entry["enabled"] == true)
        .collect();
    let manual = config
        .pointer("/metadataGeneration/providers")
        .and_then(Value::as_array);
    let mut candidates = Vec::new();
    if let Some(manual) = manual.filter(|providers| !providers.is_empty()) {
        candidates.extend(
            manual
                .iter()
                .filter_map(|input| configured_candidate(input, &enabled)),
        );
    } else {
        for (needle, preferred) in [
            ("haiku", None),
            ("gpt-5.4-mini", Some("low")),
            ("minimax-m3", None),
            ("nemotron-3-super", None),
        ] {
            let matched = enabled.iter().find_map(|entry| {
                models(entry)
                    .iter()
                    .find(|model| {
                        ["id", "label"].iter().any(|key| {
                            text(model, key)
                                .is_some_and(|value| value.to_lowercase().contains(needle))
                        })
                    })
                    .map(|model| (*entry, model))
            });
            if let Some((entry, model)) = matched {
                if let (Some(provider), Some(id)) = (text(entry, "provider"), text(model, "id")) {
                    candidates.push(Candidate {
                        provider: provider.to_owned(),
                        model: Some(id.to_owned()),
                        thinking_option_id: thinking(Some(model), preferred),
                    });
                }
            }
        }
        if let Some(selection) =
            selection.and_then(|selection| configured_candidate(selection, &enabled))
        {
            candidates.push(selection);
        }
    }
    let mut deduped = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        if !deduped.contains(&candidate) {
            deduped.push(candidate);
        }
    }
    deduped
}

fn extract_json(text: &str) -> &str {
    static FENCE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"```(?:json)?\s*\n([\s\S]*?)\n```").expect("constant JSON fence expression")
    });
    if let Some(captures) = FENCE.captures(text) {
        return captures.get(1).expect("fence capture").as_str().trim();
    }
    let source = text.trim();
    for (start, open) in source
        .bytes()
        .enumerate()
        .filter(|(_, byte)| matches!(byte, b'{' | b'['))
    {
        let close = if open == b'{' { b'}' } else { b']' };
        let mut depth = 0_i64;
        let mut in_string = false;
        let mut escaped = false;
        for (offset, byte) in source.as_bytes()[start..].iter().copied().enumerate() {
            if in_string {
                if escaped {
                    escaped = false;
                } else if byte == b'\\' {
                    escaped = true;
                } else if byte == b'"' {
                    in_string = false;
                }
                continue;
            }
            if byte == b'"' {
                in_string = true;
            } else if byte == open {
                depth += 1;
            } else if byte == close {
                depth -= 1;
                if depth == 0 {
                    let candidate = &source[start..=start + offset];
                    if serde_json::from_str::<Value>(candidate).is_ok() {
                        return candidate;
                    }
                }
            }
        }
    }
    source
}

pub(crate) fn compile_schema(schema: &Value) -> Result<jsonschema::Validator> {
    jsonschema::options()
        .with_draft(jsonschema::Draft::Draft7)
        .should_validate_formats(false)
        .offline()
        .build(schema)
        .map_err(|error| anyhow!("Invalid structured generation JSON schema: {error}"))
}

pub(crate) fn validate_response(
    response: &str,
    validator: &jsonschema::Validator,
) -> std::result::Result<Value, Vec<String>> {
    let value: Value = serde_json::from_str(extract_json(response))
        .map_err(|error| vec![format!("Invalid JSON: {error}")])?;
    let errors: Vec<_> = validator
        .iter_errors(&value)
        .map(|error| {
            let path = error.instance_path().to_string();
            format!(
                "{}: {error}",
                if path.is_empty() { "(root)" } else { &path }
            )
        })
        .collect();
    if errors.is_empty() {
        Ok(value)
    } else {
        Err(errors)
    }
}

pub(crate) fn retry_prompt(base: &str, errors: &[String]) -> String {
    let mut prompt = format!("{base}\n\nPrevious response was invalid with validation errors:\n");
    if errors.is_empty() {
        prompt.push_str("- Unknown validation error\n");
    } else {
        for error in errors {
            prompt.push_str("- ");
            prompt.push_str(error);
            prompt.push('\n');
        }
    }
    prompt.push_str("\nRespond again with JSON only that matches the schema.");
    prompt
}

impl AgentManager {
    /// Callers without a focused client selection use only the configured waterfall.
    pub async fn generate_structured(
        &self,
        cwd: &Path,
        prompt: &str,
        schema: &Value,
        schema_name: &str,
        title: &str,
        cancel: CancellationToken,
    ) -> Result<Value> {
        self.generate_structured_with_selection(
            cwd,
            prompt,
            schema,
            schema_name,
            title,
            None,
            cancel,
        )
        .await
    }

    /// Focus belongs to the requesting client, not to the manager or directory.
    pub async fn focused_generation_selection(
        &self,
        cwd: &Path,
        focused_agent_id: Option<&str>,
    ) -> Result<Option<Value>> {
        let Some(id) = focused_agent_id else {
            return Ok(None);
        };
        let Some(snapshot) = self.agent_snapshot(id).await? else {
            return Ok(None);
        };
        if snapshot["cwd"].as_str() != cwd.to_str() || snapshot["_live"] == false {
            return Ok(None);
        }
        let config = &snapshot["config"];
        let runtime = &snapshot["runtimeInfo"];
        let mut selection =
            json!({"provider":snapshot.get("provider").or_else(|| config.get("provider"))});
        for key in ["model", "thinkingOptionId"] {
            selection[key] = runtime
                .get(key)
                .filter(|value| !value.is_null())
                .or_else(|| config.get(key))
                .cloned()
                .unwrap_or(Value::Null);
        }
        Ok(Some(selection))
    }

    pub async fn generate_structured_with_selection(
        &self,
        cwd: &Path,
        prompt: &str,
        schema: &Value,
        schema_name: &str,
        title: &str,
        current_selection: Option<&Value>,
        cancel: CancellationToken,
    ) -> Result<Value> {
        if cancel.is_cancelled() {
            return Err(StructuredGenerationCancelled.into());
        }
        let app = self.app()?;
        let config = app.config.snapshot();
        let entries = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(StructuredGenerationCancelled.into()),
            entries = app.providers.list(Some(cwd), true) => entries?,
        };
        drop(app);
        let candidates = resolve_candidates(&entries, &config, current_selection);
        let mut attempts = Vec::with_capacity(candidates.len());
        for candidate in candidates {
            if cancel.is_cancelled() {
                return Err(StructuredGenerationCancelled.into());
            }
            let provider = self.app()?.providers.get(&candidate.provider);
            let availability = match provider {
                Ok(provider) => tokio::select! {
                    biased;
                    _ = cancel.cancelled() => return Err(StructuredGenerationCancelled.into()),
                    result = provider.is_available(json!({}), cancel.child_token()) => result,
                },
                Err(_) => Err(anyhow!(
                    "No client registered for provider '{}'",
                    candidate.provider
                )),
            };
            let (available, error) = match availability {
                Ok(available) => (available, None),
                Err(error) => (false, Some(error.to_string())),
            };
            if !available {
                tracing::warn!(
                    provider = candidate.provider,
                    model = candidate.model,
                    schema_name,
                    reason = error.as_deref().unwrap_or("unavailable"),
                    "Structured generation: skipping unavailable provider"
                );
                attempts.push(StructuredGenerationAttempt {
                    provider: candidate.provider,
                    model: candidate.model,
                    available,
                    error,
                });
                continue;
            }
            let result = self
                .generate_candidate(cwd, prompt, schema, title, &candidate, &cancel)
                .await;
            if cancel.is_cancelled() {
                return Err(StructuredGenerationCancelled.into());
            }
            match result {
                Ok(value) => {
                    if !attempts.is_empty() {
                        tracing::info!(provider = candidate.provider, model = candidate.model,
                            schema_name, prior_attempts = ?attempts,
                            "Structured generation: succeeded after fallback");
                    }
                    return Ok(value);
                }
                Err(error) => {
                    if error.is::<StructuredGenerationCancelled>() {
                        return Err(error);
                    }
                    tracing::warn!(provider = candidate.provider, model = candidate.model,
                        schema_name, error = %error, "Structured generation: provider failed");
                    attempts.push(StructuredGenerationAttempt {
                        provider: candidate.provider,
                        model: candidate.model,
                        available: true,
                        error: Some(error.to_string()),
                    });
                }
            }
        }
        if cancel.is_cancelled() {
            return Err(StructuredGenerationCancelled.into());
        }
        Err(StructuredAgentFallbackError { attempts }.into())
    }

    async fn generate_candidate(
        &self,
        cwd: &Path,
        prompt: &str,
        schema: &Value,
        title: &str,
        candidate: &Candidate,
        cancel: &CancellationToken,
    ) -> Result<Value> {
        let mut config = json!({
            "provider":candidate.provider, "cwd":cwd, "title":title, "internal":true,
        });
        if let Some(model) = &candidate.model {
            config["model"] = json!(model);
        }
        if let Some(thinking) = &candidate.thinking_option_id {
            config["thinkingOptionId"] = json!(thinking);
        }
        // Assign the id before creation, so cleanup can also remove a partially
        // created session if a provider fails after allocating native resources.
        let id = uuid::Uuid::new_v4().to_string();
        let actor = json!({"kind":"system","source":"structured-generation"});
        let result = async {
            self.create_session(config, Some(id.clone()), json!({"persistSession":false})).await?;
            if cancel.is_cancelled() { return Err(StructuredGenerationCancelled.into()); }
            let validator = compile_schema(schema)?;
            let base = format!("{}\n\nYou must respond with JSON only that matches this JSON Schema:\n{}",
                prompt.trim(), serde_json::to_string_pretty(schema)?);
            let mut attempt_prompt = base.clone();
            let mut last_response = String::new();
            let mut validation_errors = Vec::new();
            for attempt in 0..=2 {
                let before = self.timeline_items(&id).await?.len();
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => return Err(StructuredGenerationCancelled.into()),
                    result = self.send_immediate(&actor, &id, json!({"prompt":attempt_prompt})) => { result?; }
                }
                let completed = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => return Err(StructuredGenerationCancelled.into()),
                    result = self.wait(&actor, &id, json!({}), cancel.child_token()) => result?,
                };
                if !completed["permission"].is_null() {
                    return Err(anyhow!("Structured generation requires permission"));
                }
                match completed["status"].as_str() {
                    Some("error") => {
                        let snapshot = self.agent_snapshot(&id).await?.unwrap_or(Value::Null);
                        return Err(anyhow!("{}", snapshot["lastError"].as_str().unwrap_or("Structured generation turn failed")));
                    }
                    Some("closed") => return Err(anyhow!("Structured generation session closed during its turn")),
                    _ => {}
                }
                // Use only this turn's messages. A repair producing no assistant
                // output must not accidentally reuse the prior invalid response.
                let timeline = self.timeline_items(&id).await?;
                last_response = timeline.get(before..).unwrap_or_default().iter().rev()
                    .find(|item| item["type"] == "assistant_message")
                    .and_then(|item| item["text"].as_str()).unwrap_or("").to_owned();
                match validate_response(&last_response, &validator) {
                    Ok(value) => return Ok(value),
                    Err(errors) => validation_errors = errors,
                }
                if attempt < 2 { attempt_prompt = retry_prompt(&base, &validation_errors); }
            }
            Err(StructuredAgentResponseError { last_response, validation_errors }.into())
        }.await;
        // Cleanup is deliberately not raced against cancellation. Each native
        // session must be closed, and its transient manager state removed.
        let _ = self.control(&actor, &id, "close", json!({})).await;
        let _ = self.control(&actor, &id, "delete", json!({})).await;
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_candidates_exclude_defaults_and_current_selection() {
        let entries = vec![json!({
            "provider":"claude", "enabled":true,
            "models":[{"id":"claude-haiku", "label":"Haiku"}]
        })];
        let config = json!({"metadataGeneration":{"providers":[
            {"provider":" local ", "model":" custom ", "thinkingOptionId":"high"},
            {"provider":"local", "model":"custom", "thinkingOptionId":"high"}
        ]}});
        assert_eq!(
            resolve_candidates(&entries, &config, Some(&json!({"provider":"claude"}))),
            vec![Candidate {
                provider: "local".into(),
                model: Some("custom".into()),
                thinking_option_id: Some("high".into())
            }]
        );
    }

    #[test]
    fn automatic_order_and_thinking_defaults_match_catalog() {
        let entries = vec![
            json!({"provider":"disabled","enabled":false,"models":[{"id":"haiku","label":"Haiku"}]}),
            json!({"provider":"router","enabled":true,"models":[
                {"id":"nemotron-3-super","label":"Super"},
                {"id":"gpt-5.4-mini","label":"Mini","thinkingOptions":[{"id":"low"}],"defaultThinkingOptionId":"medium"},
                {"id":"minimax-m3","label":"M3"},
                {"id":"claude-haiku","label":"Haiku"}
            ]}),
            json!({"provider":"focused","enabled":true,"models":[
                {"id":"first","label":"First"},
                {"id":"preferred","label":"Preferred","isDefault":true,"defaultThinkingOptionId":"balanced"}
            ]}),
        ];
        let selected =
            resolve_candidates(&entries, &json!({}), Some(&json!({"provider":"focused"})));
        assert_eq!(
            selected
                .iter()
                .map(|candidate| candidate.model.as_deref().unwrap())
                .collect::<Vec<_>>(),
            [
                "claude-haiku",
                "gpt-5.4-mini",
                "minimax-m3",
                "nemotron-3-super",
                "preferred"
            ]
        );
        assert_eq!(selected[1].thinking_option_id.as_deref(), Some("low"));
        assert_eq!(selected[4].thinking_option_id.as_deref(), Some("balanced"));
    }

    #[test]
    fn nested_aliases_and_error_state_explicit_models_are_preserved() {
        let entries = vec![
            json!({"provider":"router","enabled":true,"models":[{
                "id":"plexus/small-fast","label":"Small",
                "metadata":{"providerId":"plexus","modelId":"small-fast"},
                "defaultThinkingOptionId":"balanced"
            }]}),
            json!({"provider":"broken","enabled":true,"status":"error"}),
        ];
        let config = json!({"metadataGeneration":{"providers":[
            {"provider":"PLEXUS","model":"SMALL-FAST","thinkingOptionId":"unsupported"},
            {"provider":"broken","model":"explicit","thinkingOptionId":"unchanged"}
        ]}});
        let selected = resolve_candidates(&entries, &config, None);
        assert_eq!(
            selected[0],
            Candidate {
                provider: "router".into(),
                model: Some("plexus/small-fast".into()),
                thinking_option_id: Some("balanced".into())
            }
        );
        assert_eq!(
            selected[1],
            Candidate {
                provider: "broken".into(),
                model: Some("explicit".into()),
                thinking_option_id: Some("unchanged".into())
            }
        );
        assert_eq!(
            resolve_candidates(&[], &json!({}), Some(&json!({"provider":"absent"})))[0].provider,
            "absent"
        );
    }

    #[test]
    fn extracts_fences_prose_arrays_and_escaped_strings() {
        for (input, expected) in [
            ("```json\n{\"value\":42}\n```", "{\"value\":42}"),
            ("```\n{\"value\":42}\n```", "{\"value\":42}"),
            (
                "prefix [not JSON] then {\"value\":[\"}\",\"é\"]} suffix",
                "{\"value\":[\"}\",\"é\"]}",
            ),
            (
                "before [1,{\"text\":\"escaped \\\" quote\"}] after",
                "[1,{\"text\":\"escaped \\\" quote\"}]",
            ),
            ("true", "true"),
        ] {
            assert_eq!(extract_json(input), expected);
        }
        // A matched fence has priority, even when a later object is valid.
        assert_eq!(extract_json("```json\nbad\n``` {\"valid\":true}"), "bad");
    }

    #[test]
    fn validates_schema_and_builds_non_accumulating_repair_prompts() {
        let validator = compile_schema(&json!({
            "type":"object", "properties":{"count":{"type":"integer","minimum":1}},
            "required":["count"], "additionalProperties":false
        }))
        .unwrap();
        let parse_errors = validate_response("not json", &validator).unwrap_err();
        assert!(parse_errors[0].starts_with("Invalid JSON: "));
        let schema_errors =
            validate_response("{\"count\":0,\"extra\":true}", &validator).unwrap_err();
        assert_eq!(schema_errors.len(), 2);
        let retry = retry_prompt("Original request", &schema_errors);
        assert!(retry.starts_with(
            "Original request\n\nPrevious response was invalid with validation errors:\n"
        ));
        assert!(retry.contains("count"));
        assert!(retry.ends_with("Respond again with JSON only that matches the schema."));
        assert_eq!(
            validate_response("{\"count\":2}", &validator).unwrap(),
            json!({"count":2})
        );
    }
}
