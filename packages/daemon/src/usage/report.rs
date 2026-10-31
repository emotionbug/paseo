use anyhow::{Result, anyhow, bail};
use serde_json::{Map, Value, json};

#[derive(Clone, Copy)]
enum Field {
    Text,
    NullableText,
    Number,
    NullableNumber,
    Boolean,
    Tone,
    Unit,
}
fn object(value: &Value, fields: &[(&str, Field, bool)]) -> Result<Value> {
    let input = value
        .as_object()
        .ok_or_else(|| anyhow!("Invalid usage report object"))?;
    let mut output = Map::new();
    for (name, kind, required) in fields {
        let Some(value) = input.get(*name) else {
            if *required {
                bail!("Missing usage report field: {name}");
            }
            continue;
        };
        let valid = match kind {
            Field::Text => value.is_string(),
            Field::NullableText => value.is_null() || value.is_string(),
            Field::Number => value.is_number(),
            Field::NullableNumber => value.is_null() || value.is_number(),
            Field::Boolean => value.is_boolean(),
            Field::Tone => matches!(
                value.as_str(),
                Some("default" | "ok" | "warning" | "danger")
            ),
            Field::Unit => matches!(
                value.as_str(),
                Some("usd" | "credits" | "requests" | "tokens")
            ),
        };
        if !valid {
            bail!("Invalid usage report field: {name}");
        }
        output.insert((*name).to_owned(), value.clone());
    }
    Ok(Value::Object(output))
}
fn list(
    report: &Value,
    name: &str,
    required: bool,
    fields: &[(&str, Field, bool)],
) -> Result<Option<Value>> {
    let Some(value) = report.get(name) else {
        if required {
            bail!("Missing usage report field: {name}");
        }
        return Ok(None);
    };
    let values = value
        .as_array()
        .ok_or_else(|| anyhow!("Invalid usage report field: {name}"))?;
    Ok(Some(Value::Array(
        values
            .iter()
            .map(|value| object(value, fields))
            .collect::<Result<_>>()?,
    )))
}

/// The canonical UsageReportSchema strips unknown keys, including plugin-owned data.
pub(super) fn validate(report: Value) -> Result<Value> {
    use Field::*;
    match report["status"].as_str() {
        Some("available") => {
            let mut result = object(
                &report,
                &[("status", Text, true), ("planLabel", Text, false)],
            )?;
            result["windows"] = list(
                &report,
                "windows",
                true,
                &[
                    ("id", Text, true),
                    ("label", Text, true),
                    ("shortLabel", Text, false),
                    ("summary", Boolean, false),
                    ("usedPct", NullableNumber, false),
                    ("remainingPct", NullableNumber, false),
                    ("resetsAt", NullableText, false),
                    ("runsOutAt", NullableText, false),
                    ("shortfallPct", NullableNumber, false),
                    ("tone", Tone, false),
                ],
            )?
            .ok_or_else(|| anyhow!("Missing usage report windows"))?;
            if let Some(balances) = list(
                &report,
                "balances",
                false,
                &[
                    ("id", Text, true),
                    ("label", Text, true),
                    ("used", NullableNumber, false),
                    ("remaining", NullableNumber, false),
                    ("limit", NullableNumber, false),
                    ("unit", Unit, true),
                    ("resetsAt", NullableText, false),
                    ("tone", Tone, false),
                ],
            )? {
                result["balances"] = balances;
            }
            if let Some(details) = list(
                &report,
                "details",
                false,
                &[
                    ("id", Text, true),
                    ("label", Text, true),
                    ("value", Text, true),
                    ("tone", Tone, false),
                ],
            )? {
                result["details"] = details;
            }
            Ok(result)
        }
        Some("error") => object(&report, &[("status", Text, true), ("error", Text, true)]),
        Some("unavailable") => {
            let input = &report["problem"];
            let problem = match input["kind"].as_str() {
                Some("expired") => {
                    let problem = object(
                        input,
                        &[
                            ("kind", Text, true),
                            ("expiresAt", Text, true),
                            ("refreshedBy", Text, false),
                        ],
                    )?;
                    let date = problem["expiresAt"].as_str().unwrap();
                    if !date.ends_with('Z') || chrono::DateTime::parse_from_rfc3339(date).is_err() {
                        bail!("Invalid usage report field: expiresAt");
                    }
                    problem
                }
                Some("rejected") => {
                    let problem = object(
                        input,
                        &[
                            ("kind", Text, true),
                            ("status", Number, true),
                            ("refreshedBy", Text, false),
                        ],
                    )?;
                    if !problem["status"]
                        .as_f64()
                        .is_some_and(|value| value.fract() == 0.0)
                    {
                        bail!("Invalid usage report field: status");
                    }
                    problem
                }
                Some("no_quota") => object(input, &[("kind", Text, true), ("detail", Text, true)])?,
                _ => bail!("Invalid usage problem kind"),
            };
            Ok(json!({"status":"unavailable","problem":problem}))
        }
        _ => bail!("Invalid usage report status"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn plugin_reports_drop_unknown_data_but_validate_optional_fields() {
        let report = validate(json!({"status":"available","private":"credential",
            "windows":[{"id":"window","label":"Window","private":"credential","usedPct":null}]}))
        .unwrap();
        assert!(report.get("private").is_none());
        assert!(report["windows"][0].get("private").is_none());
        assert!(validate(json!({"status":"available","windows":[{"id":"window","label":"Window","usedPct":"12"}]})).is_err());
        assert!(validate(json!({"status":"available","windows":[],"balances":[{"id":"x","label":"X","unit":"money"}]})).is_err());
    }
    #[test]
    fn unavailable_reports_require_their_actual_problem_payload() {
        assert!(validate(json!({"status":"unavailable","problem":{"kind":"expired"}})).is_err());
        assert!(
            validate(json!({"status":"unavailable","problem":{"kind":"rejected","status":401.5}}))
                .is_err()
        );
        assert_eq!(validate(json!({"status":"unavailable","problem":{"kind":"no_quota","detail":"None","extra":true}})).unwrap(),
            json!({"status":"unavailable","problem":{"kind":"no_quota","detail":"None"}}));
    }
}
