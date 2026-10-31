use std::{collections::BTreeMap, fmt, sync::LazyLock};

use anyhow::{Result, anyhow};
use jsonschema::{ValidationError, Validator, error::ValidationErrorKind};
use serde_json::{Value, json};

#[derive(Debug)]
pub struct ProviderOptionsValidationError {
    pub provider: String,
    pub issues: Value,
}

impl ProviderOptionsValidationError {
    pub const CODE: &'static str = "provider_options_invalid";
}

impl fmt::Display for ProviderOptionsValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "Invalid providerOptions for '{}': ",
            self.provider
        )?;
        if let Some(issues) = self.issues.as_array() {
            for (index, issue) in issues.iter().enumerate() {
                if index != 0 {
                    formatter.write_str("; ")?;
                }
                formatter.write_str("providerOptions")?;
                if let Some(path) = issue.get("path").and_then(Value::as_array) {
                    for segment in path {
                        if let Some(segment) = segment.as_str() {
                            let mut bytes = segment.bytes();
                            let identifier = bytes
                                .next()
                                .is_some_and(|c| c.is_ascii_alphabetic() || c == b'_' || c == b'$')
                                && bytes
                                    .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'$');
                            if identifier {
                                write!(formatter, ".{segment}")?;
                            } else {
                                write!(formatter, "[{}]", json!(segment))?;
                            }
                        } else {
                            write!(formatter, "[{segment}]")?;
                        }
                    }
                }
                write!(
                    formatter,
                    ": {}",
                    issue
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("Invalid value")
                )?;
            }
        }
        Ok(())
    }
}

impl std::error::Error for ProviderOptionsValidationError {}

#[derive(Debug)]
pub struct ToolPolicyUnsupportedError {
    pub provider: String,
    reason: Option<String>,
}

impl ToolPolicyUnsupportedError {
    pub const CODE: &'static str = "tool_policy_unsupported";
    pub fn new(provider: impl Into<String>, reason: Option<String>) -> Self {
        Self {
            provider: provider.into(),
            reason,
        }
    }
}

impl fmt::Display for ToolPolicyUnsupportedError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(reason) = &self.reason {
            formatter.write_str(reason)
        } else {
            write!(
                formatter,
                "Provider '{}' cannot preapprove exact MCP tools for unattended execution; select Claude, Codex, or OpenCode",
                self.provider
            )
        }
    }
}

impl std::error::Error for ToolPolicyUnsupportedError {}

static VALIDATORS: LazyLock<BTreeMap<String, Validator>> = LazyLock::new(|| {
    let schemas: BTreeMap<String, Value> =
        serde_json::from_str(include_str!("options.schema.json"))
            .expect("embedded provider option schemas");
    schemas
        .into_iter()
        .map(|(family, schema)| {
            let validator = jsonschema::options()
                .should_validate_formats(false)
                .build(&schema)
                .expect("valid embedded provider option schema");
            (family, validator)
        })
        .collect()
});

fn issue_path(input: &Value, pointer: &str) -> Vec<Value> {
    let mut path = Vec::new();
    let mut current = input;
    for part in pointer.split('/').skip(1) {
        let part = part.replace("~1", "/").replace("~0", "~");
        if current.is_array() {
            if let Ok(index) = part.parse::<usize>() {
                path.push(json!(index));
                current = current.get(index).unwrap_or(&Value::Null);
                continue;
            }
        }
        current = current.get(&part).unwrap_or(&Value::Null);
        path.push(Value::String(part));
    }
    path
}

fn flatten(error: &ValidationError<'_>, input: &Value, issues: &mut Vec<Value>) {
    match error.kind() {
        ValidationErrorKind::AnyOf { context } | ValidationErrorKind::OneOfNotValid { context } => {
            for alternatives in context {
                for error in alternatives {
                    flatten(error, input, issues);
                }
            }
        }
        _ => {
            let mut path = issue_path(input, error.instance_path().as_str());
            if let ValidationErrorKind::Required { property } = error.kind() {
                path.push(property.clone());
            }
            issues.push(json!({"path":path,"message":error.masked().to_string()}));
        }
    }
}

/// Family is resolved by the registry; a custom public ID cannot bypass the
/// base provider's strict schema. JSON schemas preserve the original Zod input
/// contract, while the two source transformations remain explicit below.
pub fn validate(provider: &str, family: &str, options: &Value) -> Result<Value> {
    let schema_family = match family {
        "copilot" | "cursor" | "kimi" | "kiro" | "traecli" | "acp" => "acp",
        other => other,
    };
    let mut issues = Vec::new();
    if let Some(validator) = VALIDATORS.get(schema_family) {
        for error in validator.iter_errors(options) {
            flatten(&error, options, &mut issues);
        }
    } else if !options.is_object() {
        issues.push(json!({"path":[],"message":"Invalid input: expected object"}));
    }
    if !issues.is_empty() {
        if matches!(family, "claude" | "codex" | "opencode") {
            return Err(ProviderOptionsValidationError {
                provider: family.to_owned(),
                issues: Value::Array(issues),
            }
            .into());
        }
        return Err(anyhow!(
            "Invalid providerOptions for '{provider}': {}",
            Value::Array(issues)
        ));
    }
    let mut normalized = options.clone();
    if family == "pi" {
        let object = normalized.as_object_mut().expect("validated Pi object");
        object.entry("rpcTimeoutMs").or_insert(json!(60_000));
        object.entry("extensionTimeoutMs").or_insert(json!(30_000));
    }
    if schema_family == "acp" {
        if let Some(capabilities) = normalized
            .get_mut("clientCapabilities")
            .and_then(Value::as_object_mut)
        {
            capabilities.retain(|key, _| matches!(key.as_str(), "fs" | "terminal"));
            if let Some(files) = capabilities.get_mut("fs").and_then(Value::as_object_mut) {
                files.retain(|key, _| matches!(key.as_str(), "readTextFile" | "writeTextFile"));
            }
        }
    }
    Ok(normalized)
}

/// Objects merge recursively; arrays, primitives and explicit null replace.
pub fn merge(base: &Value, replacement: &Value) -> Value {
    match (base.as_object(), replacement.as_object()) {
        (Some(base), Some(replacement)) => {
            let mut result = base.clone();
            for (key, value) in replacement {
                let merged = match result.get(key) {
                    Some(previous) if previous.is_object() && value.is_object() => {
                        merge(previous, value)
                    }
                    _ => value.clone(),
                };
                result.insert(key.clone(), merged);
            }
            Value::Object(result)
        }
        _ => replacement.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_nested_privilege_typo_with_structured_source_error() {
        let error = validate(
            "custom-claude",
            "claude",
            &json!({"sandbox":{"filesystem":{"allowWrite":"/tmp"}}}),
        )
        .unwrap_err();
        let details = error
            .downcast_ref::<ProviderOptionsValidationError>()
            .unwrap();
        assert_eq!(details.provider, "claude");
        assert_eq!(
            details.issues[0]["path"],
            json!(["sandbox", "filesystem", "allowWrite"])
        );
        assert!(
            validate(
                "claude",
                "claude",
                &json!({"sandbox":{"filesystem":{"allowWrite":["/tmp"],"disabled":false}}})
            )
            .is_ok()
        );
    }

    #[test]
    fn acp_keeps_extension_options_but_strips_unknown_reverse_capabilities() {
        let options = validate("custom","acp",&json!({"extensionFlag":false,"supportsMcpServers":false,"clientCapabilities":{"terminal":false,"extra":true,"fs":{"readTextFile":false,"writeOther":true}}})).unwrap();
        assert_eq!(
            options,
            json!({"extensionFlag":false,"supportsMcpServers":false,"clientCapabilities":{"terminal":false,"fs":{"readTextFile":false}}})
        );
    }

    #[test]
    fn nested_option_override_does_not_merge_arrays_or_resurrect_null() {
        assert_eq!(
            merge(
                &json!({"a":{"x":1,"y":[1,2]},"b":{"x":1}}),
                &json!({"a":{"y":[3]},"b":null})
            ),
            json!({"a":{"x":1,"y":[3]},"b":null})
        );
    }
}
