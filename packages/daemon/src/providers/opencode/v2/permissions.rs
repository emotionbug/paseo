use super::{array, s};
use anyhow::{Result, bail};
use serde_json::{Map, Value, json};

pub(super) fn permission(request: &Value) -> Value {
    let description = request
        .get("message")
        .filter(|v| !v.is_null())
        .cloned()
        .unwrap_or_else(|| {
            Value::String(
                array(&request["resources"])
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join("\n"),
            )
        });
    json!({"id":request["id"],"provider":"opencode","kind":"tool","name":request["action"],"description":description,
        "actions":[{"id":"once","label":"Allow once","behavior":"allow"},{"id":"always","label":"Always allow","behavior":"allow"},{"id":"reject","label":"Deny","behavior":"deny"}]})
}
pub(super) fn form(form: &Value) -> Value {
    let questions: Vec<Value> = array(&form["fields"]).iter().map(|field| {
        let header = field.get("title").filter(|v|!v.is_null()).unwrap_or(&field["key"]);
        let mut q=json!({"header":header,"question":field.get("description").filter(|v|!v.is_null()).unwrap_or(header),"multiSelect":s(field,"type")=="multiselect","allowOther":field["custom"]==true});
        if let Some(options)=field.get("options") {q["options"]=options.clone();} q
    }).collect();
    let mut request = json!({"id":form["id"],"provider":"opencode","name":"question","kind":"question","input":{"questions":questions}});
    if let Some(title) = form.get("title") {
        request["title"] = title.clone();
    }
    request
}
pub(super) fn answer(form: &Value, response: &Value) -> Result<Value> {
    let raw = response["updatedInput"]["answers"]
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("OpenCode question response requires answers"))?;
    let mut answer = Map::new();
    for field in array(&form["fields"]) {
        let key = s(field, "key");
        let Some(value) = raw
            .get(key)
            .filter(|v| !v.is_null())
            .or_else(|| raw.get(field["title"].as_str().unwrap_or(key)))
        else {
            continue;
        };
        if s(field, "type") == "external" {
            continue;
        }
        let normalize = |label: &str| -> Value {
            array(&field["options"])
                .iter()
                .find(|v| s(v, "value") == label)
                .or_else(|| {
                    array(&field["options"])
                        .iter()
                        .find(|v| s(v, "label") == label)
                })
                .map(|v| v["value"].clone())
                .unwrap_or_else(|| Value::String(label.to_owned()))
        };
        let normalized = match s(field, "type") {
            "string" => value.as_str().map(normalize),
            "boolean" => match value {
                Value::Bool(_) => Some(value.clone()),
                Value::String(v) if v == "true" => Some(json!(true)),
                Value::String(v) if v == "false" => Some(json!(false)),
                _ => None,
            },
            "number" | "integer" => {
                let number = value.as_f64().or_else(|| value.as_str().and_then(number));
                number
                    .filter(|n| {
                        n.is_finite() && (s(field, "type") != "integer" || n.fract() == 0.0)
                    })
                    .and_then(serde_json::Number::from_f64)
                    .map(Value::Number)
            }
            "multiselect" => {
                let selected = if let Some(items) = value.as_array() {
                    if items.iter().all(Value::is_string) {
                        Some(
                            items
                                .iter()
                                .filter_map(Value::as_str)
                                .map(str::to_owned)
                                .collect::<Vec<_>>(),
                        )
                    } else {
                        None
                    }
                } else if let Some(text) = value.as_str() {
                    let mut remaining = text;
                    let mut selected: Vec<String> = Vec::new();
                    let mut labels: Vec<&str> = array(&field["options"])
                        .iter()
                        .filter_map(|v| v["label"].as_str())
                        .collect();
                    labels.sort_by_key(|v| std::cmp::Reverse(v.len()));
                    while !remaining.is_empty() {
                        let found = labels.iter().copied().find(|label| {
                            !selected.iter().any(|v| v == label)
                                && (remaining == *label
                                    || remaining
                                        .strip_prefix(*label)
                                        .is_some_and(|v| v.starts_with(", ")))
                        });
                        let Some(label) = found else {
                            break;
                        };
                        selected.push(label.to_owned());
                        remaining = if remaining == label {
                            ""
                        } else {
                            &remaining[label.len() + 2..]
                        };
                    }
                    if remaining.is_empty() {
                        Some(selected)
                    } else if field["custom"] == true {
                        selected.push(remaining.to_owned());
                        Some(selected)
                    } else {
                        None
                    }
                } else {
                    None
                };
                selected.map(|items| Value::Array(items.iter().map(|v| normalize(v)).collect()))
            }
            _ => None,
        };
        let Some(normalized) = normalized else {
            bail!("Invalid answer for OpenCode question {key}");
        };
        answer.insert(key.to_owned(), normalized);
    }
    Ok(Value::Object(answer))
}
fn number(value: &str) -> Option<f64> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    for (prefix, radix) in [
        ("0x", 16),
        ("0X", 16),
        ("0o", 8),
        ("0O", 8),
        ("0b", 2),
        ("0B", 2),
    ] {
        if let Some(digits) = value.strip_prefix(prefix) {
            if digits.is_empty() {
                return None;
            }
            return digits.chars().try_fold(0.0, |total, digit| {
                digit
                    .to_digit(radix)
                    .map(|digit| total * radix as f64 + digit as f64)
            });
        }
    }
    value.parse().ok()
}
