//! Native counterpart of the SDK's ProviderInput/ProviderEvent schemas. Unknown
//! event fields are stripped; strict input objects and exact MCP grants are not.
use anyhow::{Context, Result, bail};
use serde_json::Value;

type Normalize = fn(&mut Value) -> Result<()>;
#[derive(Clone, Copy)]
enum Field {
    String,
    Id,
    Number,
    Natural,
    Positive,
    Boolean,
    Json,
    Object,
    Null,
    NullableString,
    NullableNumber,
    PositiveOrNull,
    Enum(&'static [&'static str]),
    Nested(Normalize),
    Array(Normalize),
    Strings,
    StringMap,
}
use Field::*;
fn field(value: &mut Value, kind: Field) -> Result<()> {
    let valid = match kind {
        String => value.is_string(),
        Id => value.as_str().is_some_and(|s| !s.is_empty()),
        Number => value.is_number(),
        Natural => value.as_f64().is_some_and(|n| n >= 0.0 && n.fract() == 0.0),
        Positive => value.as_f64().is_some_and(|n| n > 0.0 && n.fract() == 0.0),
        Boolean => value.is_boolean(),
        Json => true,
        Object => value.is_object(),
        Null => value.is_null(),
        NullableString => value.is_null() || value.is_string(),
        NullableNumber => value.is_null() || value.is_number(),
        PositiveOrNull => {
            value.is_null() || value.as_f64().is_some_and(|n| n > 0.0 && n.fract() == 0.0)
        }
        Enum(values) => value.as_str().is_some_and(|s| values.contains(&s)),
        Nested(normalize) => return normalize(value),
        Array(normalize) => {
            for item in value.as_array_mut().context("Expected array")? {
                normalize(item)?;
            }
            true
        }
        Strings => value
            .as_array()
            .is_some_and(|values| values.iter().all(Value::is_string)),
        StringMap => value
            .as_object()
            .is_some_and(|values| values.values().all(Value::is_string)),
    };
    if !valid {
        bail!("Invalid provider protocol value: {value}");
    }
    Ok(())
}
fn object(value: &mut Value, fields: &[(&str, Field)], strict: bool) -> Result<()> {
    let object = value
        .as_object_mut()
        .context("Expected provider protocol object")?;
    for (key, kind) in fields {
        let name = key.strip_suffix('?').unwrap_or(key);
        if let Some(value) = object.get_mut(name) {
            field(value, *kind).with_context(|| format!("Invalid provider field {name}"))?;
        } else if name == *key {
            bail!("Missing provider field {name}");
        }
    }
    if strict
        && object.keys().any(|key| {
            !fields
                .iter()
                .any(|(name, _)| name.trim_end_matches('?') == key)
        })
    {
        bail!("Unknown provider protocol field");
    }
    object.retain(|key, _| {
        fields
            .iter()
            .any(|(name, _)| name.trim_end_matches('?') == key)
    });
    Ok(())
}
fn json_object(value: &mut Value) -> Result<()> {
    field(value, Object)
}
fn persistence(value: &mut Value) -> Result<()> {
    object(value, &[("version", Natural), ("data", Json)], false)
}
fn error(value: &mut Value) -> Result<()> {
    object(
        value,
        &[
            ("message", String),
            ("code?", String),
            ("diagnostic?", String),
        ],
        false,
    )
}
fn discriminator(value: &Value) -> Result<&str> {
    value["type"].as_str().context("Missing provider type")
}
fn mcp(value: &mut Value) -> Result<()> {
    match discriminator(value)? {
        "stdio" => object(
            value,
            &[
                ("type", String),
                ("command", String),
                ("args?", Strings),
                ("env?", StringMap),
                ("alwaysLoad?", Boolean),
            ],
            true,
        ),
        "http" | "sse" => object(
            value,
            &[
                ("type", String),
                ("url", String),
                ("headers?", StringMap),
                ("alwaysLoad?", Boolean),
            ],
            true,
        ),
        _ => bail!("Invalid provider MCP server type"),
    }
}
fn mcp_servers(value: &mut Value) -> Result<()> {
    for server in value
        .as_object_mut()
        .context("mcpServers must be an object")?
        .values_mut()
    {
        mcp(server)?;
    }
    Ok(())
}
fn grant(value: &mut Value) -> Result<()> {
    object(
        value,
        &[("kind", Enum(&["mcp"])), ("server", Id), ("tool", Id)],
        true,
    )
}
fn tool_policy(value: &mut Value) -> Result<()> {
    object(value, &[("preapproved", Array(grant))], true)
}
pub(super) fn session_config(value: &mut Value) -> Result<()> {
    object(
        value,
        &[
            ("cwd", String),
            ("env", StringMap),
            ("systemPrompt?", String),
            ("mcpServers", Nested(mcp_servers)),
            ("toolPolicy?", Nested(tool_policy)),
            ("model?", String),
            ("mode?", String),
            ("thinkingOption?", String),
            ("settings", Object),
            ("providerOptions?", Object),
            ("title?", String),
            ("persist", Boolean),
        ],
        true,
    )
}
fn external_resource(value: &mut Value) -> Result<()> {
    object(
        value,
        &[
            ("provider", String),
            ("providerLabel", String),
            ("resourceType", String),
            ("id", String),
            ("identifier", String),
            ("title", String),
            ("url", String),
        ],
        false,
    )
}
fn review_line(value: &mut Value) -> Result<()> {
    object(
        value,
        &[
            ("oldLineNumber", PositiveOrNull),
            ("newLineNumber", PositiveOrNull),
            ("type", Enum(&["add", "remove", "context"])),
            ("content", String),
        ],
        false,
    )
}
fn review_context(value: &mut Value) -> Result<()> {
    object(
        value,
        &[
            ("hunkHeader", String),
            ("targetLine", Nested(review_line)),
            ("lines", Array(review_line)),
        ],
        false,
    )
}
fn review_comment(value: &mut Value) -> Result<()> {
    object(
        value,
        &[
            ("filePath", String),
            ("side", Enum(&["old", "new"])),
            ("lineNumber", Positive),
            ("body", String),
            ("context", Nested(review_context)),
        ],
        false,
    )
}
fn content_part(value: &mut Value) -> Result<()> {
    match discriminator(value)? {
        "text" if value.get("mimeType").is_some() => object(
            value,
            &[
                ("type", String),
                ("mimeType", Enum(&["text/plain"])),
                ("contextKind?", String),
                ("title?", NullableString),
                ("text", String),
                ("externalResource?", Nested(external_resource)),
            ],
            false,
        ),
        "text" => object(value, &[("type", String), ("text", String)], true),
        "image" => object(
            value,
            &[("type", String), ("data", String), ("mimeType", String)],
            true,
        ),
        "forge_change_request" => object(
            value,
            &[
                ("type", String),
                (
                    "mimeType",
                    Enum(&["application/paseo-forge-change-request"]),
                ),
                ("forge?", String),
                ("number", Positive),
                ("title", String),
                ("url", String),
                ("body?", NullableString),
                ("projectPath?", String),
                ("baseRefName?", NullableString),
                ("headRefName?", NullableString),
            ],
            false,
        ),
        "forge_issue" => object(
            value,
            &[
                ("type", String),
                ("mimeType", Enum(&["application/paseo-forge-issue"])),
                ("forge?", String),
                ("number", Positive),
                ("title", String),
                ("url", String),
                ("body?", NullableString),
                ("projectPath?", String),
            ],
            false,
        ),
        "github_pr" => object(
            value,
            &[
                ("type", String),
                ("mimeType", Enum(&["application/github-pr"])),
                ("number", Positive),
                ("title", String),
                ("url", String),
                ("body?", NullableString),
                ("baseRefName?", NullableString),
                ("headRefName?", NullableString),
            ],
            false,
        ),
        "github_issue" => object(
            value,
            &[
                ("type", String),
                ("mimeType", Enum(&["application/github-issue"])),
                ("number", Positive),
                ("title", String),
                ("url", String),
                ("body?", NullableString),
            ],
            false,
        ),
        "review" => object(
            value,
            &[
                ("type", String),
                ("mimeType", Enum(&["application/paseo-review"])),
                ("cwd", String),
                ("mode", Enum(&["uncommitted", "base"])),
                ("baseRef?", NullableString),
                ("comments", Array(review_comment)),
            ],
            false,
        ),
        "uploaded_file" => object(
            value,
            &[
                ("type", String),
                ("id", String),
                ("fileName", String),
                ("mimeType", String),
                ("size", Natural),
                ("path", String),
            ],
            false,
        ),
        _ => bail!("Unknown provider prompt content type"),
    }
}
pub(super) fn content(value: &mut Value) -> Result<()> {
    field(value, Array(content_part))
}
fn prompt_input(value: &mut Value) -> Result<()> {
    match discriminator(value)? {
        "message" => object(
            value,
            &[("type", String), ("content", Nested(content))],
            true,
        ),
        "command" => object(
            value,
            &[("type", String), ("name", Id), ("arguments", String)],
            true,
        ),
        _ => bail!("Invalid provider prompt input"),
    }
}
fn prompt(value: &mut Value) -> Result<()> {
    object(
        value,
        &[
            ("clientMessageId", Id),
            ("delivery", Enum(&["auto", "steer"])),
            ("input", Nested(prompt_input)),
            ("outputSchema?", Json),
            ("clearPendingPermissions?", Boolean),
        ],
        true,
    )
}
fn changes(value: &mut Value) -> Result<()> {
    object(
        value,
        &[
            ("model?", NullableString),
            ("mode?", NullableString),
            ("thinkingOption?", NullableString),
            ("settings?", Object),
        ],
        true,
    )
}
pub(super) fn permission_response(value: &mut Value) -> Result<()> {
    match value["behavior"].as_str() {
        Some("allow") => object(
            value,
            &[
                ("behavior", String),
                ("selectedActionId?", String),
                ("updatedInput?", Object),
                ("updatedPermissions?", Array(json_object)),
            ],
            false,
        ),
        Some("deny") => object(
            value,
            &[
                ("behavior", String),
                ("selectedActionId?", String),
                ("message?", String),
                ("interrupt?", Boolean),
            ],
            false,
        ),
        _ => bail!("Invalid provider permission response"),
    }
}
pub(super) fn input(value: &mut Value) -> Result<()> {
    let fields: &[(&str, Field)] = match discriminator(value)? {
        "catalog" => &[("type", String), ("requestId", Id), ("cwd?", String)],
        "sessions" => &[
            ("type", String),
            ("requestId", Id),
            ("query?", String),
            ("cwd?", String),
            ("limit?", Positive),
        ],
        "session.open" => &[
            ("type", String),
            ("requestId", Id),
            ("sessionId", Id),
            ("config", Nested(session_config)),
            ("persistence?", Nested(persistence)),
            ("history", Enum(&["replay", "skip"])),
        ],
        "session.prompt" => &[
            ("type", String),
            ("sessionId", Id),
            ("prompt", Nested(prompt)),
        ],
        "session.interrupt" | "session.close" => {
            &[("type", String), ("requestId", Id), ("sessionId", Id)]
        }
        "session.permission" => &[
            ("type", String),
            ("sessionId", Id),
            ("permissionId", Id),
            ("response", Nested(permission_response)),
        ],
        "session.configure" => &[
            ("type", String),
            ("requestId", Id),
            ("sessionId", Id),
            ("changes", Nested(changes)),
        ],
        "session.revert" => &[
            ("type", String),
            ("requestId", Id),
            ("sessionId", Id),
            ("token", Json),
            ("scope", Enum(&["conversation", "files", "both"])),
        ],
        "session.archive" | "session.unarchive" => &[
            ("type", String),
            ("requestId", Id),
            ("persistence", Nested(persistence)),
        ],
        _ => bail!("Unknown provider input type"),
    };
    object(value, fields, true)
}
fn mode(value: &mut Value) -> Result<()> {
    object(
        value,
        &[
            ("id", Id),
            ("label", String),
            ("description?", String),
            ("icon?", String),
            ("colorTier?", String),
            ("isUnattended?", Boolean),
        ],
        false,
    )
}
fn thinking(value: &mut Value) -> Result<()> {
    object(
        value,
        &[
            ("id", Id),
            ("label", String),
            ("description?", String),
            ("isDefault?", Boolean),
            ("metadata?", Object),
        ],
        false,
    )
}
fn model(value: &mut Value) -> Result<()> {
    object(
        value,
        &[
            ("id", Id),
            ("aliases?", Strings),
            ("isSelectable?", Boolean),
            ("label", String),
            ("description?", String),
            ("isDefault?", Boolean),
            ("metadata?", Object),
            ("contextWindowMaxTokens?", Number),
            ("thinkingOptions?", Array(thinking)),
            ("defaultThinkingOptionId?", String),
        ],
        false,
    )
}
fn select_option(value: &mut Value) -> Result<()> {
    object(value, &[("label", String), ("value", String)], false)
}
fn setting(value: &mut Value) -> Result<()> {
    match discriminator(value)? {
        "toggle" => object(
            value,
            &[
                ("type", String),
                ("id", Id),
                ("label", String),
                ("description?", String),
                ("value", Boolean),
            ],
            false,
        ),
        "select" => object(
            value,
            &[
                ("type", String),
                ("id", Id),
                ("label", String),
                ("description?", String),
                ("value", NullableString),
                ("options", Array(select_option)),
            ],
            false,
        ),
        _ => bail!("Invalid provider setting"),
    }
}
fn config(value: &mut Value) -> Result<()> {
    object(
        value,
        &[
            ("model?", String),
            ("mode?", String),
            ("thinkingOption?", String),
            ("models", Array(model)),
            ("modes", Array(mode)),
            ("thinkingOptions", Array(thinking)),
            ("settings", Array(setting)),
        ],
        false,
    )
}
fn catalog(value: &mut Value) -> Result<()> {
    object(
        value,
        &[
            ("models", Array(model)),
            ("modes", Array(mode)),
            ("thinkingOptions?", Array(thinking)),
            ("defaultModel?", String),
            ("defaultMode?", String),
            ("defaultThinkingOption?", String),
        ],
        false,
    )
}
fn summary(value: &mut Value) -> Result<()> {
    object(
        value,
        &[
            ("persistence", Nested(persistence)),
            ("cwd", String),
            ("title?", String),
            ("description?", String),
            ("updatedAt?", String),
        ],
        false,
    )
}
fn command(value: &mut Value) -> Result<()> {
    object(
        value,
        &[
            ("name", Id),
            ("description", String),
            ("argumentHint?", String),
        ],
        false,
    )
}
fn notice(value: &mut Value) -> Result<()> {
    object(
        value,
        &[
            ("id", Id),
            ("severity", Enum(&["info", "warning", "error"])),
            ("title", String),
            ("description?", String),
            ("dismissed?", Boolean),
        ],
        false,
    )
}
fn usage(value: &mut Value) -> Result<()> {
    object(
        value,
        &[
            ("inputTokens?", Number),
            ("cachedInputTokens?", Number),
            ("outputTokens?", Number),
            ("totalCostUsd?", Number),
            ("contextWindowMaxTokens?", Number),
            ("contextWindowUsedTokens?", Number),
        ],
        false,
    )
}
fn worktree_command(value: &mut Value) -> Result<()> {
    object(
        value,
        &[
            ("index", Positive),
            ("command", String),
            ("cwd", String),
            ("log", String),
            ("status", Enum(&["running", "completed", "failed"])),
            ("exitCode", NullableNumber),
            ("durationMs?", Number),
        ],
        false,
    )?;
    if value["durationMs"].as_f64().is_some_and(|n| n < 0.0) {
        bail!("Negative worktree command duration");
    }
    Ok(())
}
fn action(value: &mut Value) -> Result<()> {
    object(
        value,
        &[
            ("index", Positive),
            ("toolName", String),
            ("summary?", String),
        ],
        false,
    )
}
fn web_result(value: &mut Value) -> Result<()> {
    object(value, &[("title", String), ("url", String)], false)
}
fn detail(value: &mut Value) -> Result<()> {
    let fields: &[(&str, Field)] = match discriminator(value)? {
        "shell" => &[
            ("type", String),
            ("command", String),
            ("cwd?", String),
            ("output?", String),
            ("exitCode?", NullableNumber),
        ],
        "read" => &[
            ("type", String),
            ("filePath", String),
            ("content?", String),
            ("offset?", Number),
            ("limit?", Number),
        ],
        "edit" => &[
            ("type", String),
            ("filePath", String),
            ("oldString?", String),
            ("newString?", String),
            ("unifiedDiff?", String),
        ],
        "write" => &[("type", String), ("filePath", String), ("content?", String)],
        "search" => &[
            ("type", String),
            ("query", String),
            ("toolName?", Enum(&["search", "grep", "glob", "web_search"])),
            ("content?", String),
            ("filePaths?", Strings),
            ("webResults?", Array(web_result)),
            ("annotations?", Strings),
            ("numFiles?", Number),
            ("numMatches?", Number),
            ("durationMs?", Number),
            ("durationSeconds?", Number),
            ("truncated?", Boolean),
            ("mode?", Enum(&["content", "files_with_matches", "count"])),
        ],
        "fetch" => &[
            ("type", String),
            ("url", String),
            ("prompt?", String),
            ("result?", String),
            ("code?", Number),
            ("codeText?", String),
            ("bytes?", Number),
            ("durationMs?", Number),
        ],
        "worktree_setup" => &[
            ("type", String),
            ("worktreePath", String),
            ("branchName", String),
            ("log", String),
            ("commands", Array(worktree_command)),
            ("truncated?", Boolean),
        ],
        "sub_agent" => &[
            ("type", String),
            ("subAgentType?", String),
            ("description?", String),
            ("childSessionId?", String),
            ("log", String),
            ("actions?", Array(action)),
        ],
        "plain_text" => &[
            ("type", String),
            ("label?", String),
            ("text?", String),
            (
                "icon?",
                Enum(&[
                    "wrench",
                    "square_terminal",
                    "eye",
                    "pencil",
                    "search",
                    "bot",
                    "sparkles",
                    "brain",
                    "mic_vocal",
                ]),
            ),
        ],
        "plan" => &[("type", String), ("text", String)],
        "unknown" => &[("type", String), ("input", Json), ("output", Json)],
        _ => bail!("Unknown provider tool detail type"),
    };
    object(value, fields, false)
}
fn todo(value: &mut Value) -> Result<()> {
    object(
        value,
        &[
            ("text", String),
            ("completed", Boolean),
            ("id?", String),
            ("status?", Enum(&["pending", "in_progress", "completed"])),
            ("activeForm?", String),
        ],
        false,
    )
}
fn timeline(value: &mut Value) -> Result<()> {
    let fields: &[(&str, Field)] = match discriminator(value)? {
        "user_message" => &[
            ("id", Id),
            ("revertToken?", Json),
            ("type", String),
            ("text", String),
            ("messageId?", String),
            ("clientMessageId?", String),
        ],
        "assistant_message" => &[
            ("id", Id),
            ("revertToken?", Json),
            ("type", String),
            ("text", String),
            ("messageId?", String),
        ],
        "reasoning" => &[
            ("id", Id),
            ("revertToken?", Json),
            ("type", String),
            ("text", String),
        ],
        "tool_call" => &[
            ("id", Id),
            ("revertToken?", Json),
            ("type", String),
            ("callId", Id),
            ("name", String),
            ("detail", Nested(detail)),
            ("metadata?", Object),
            (
                "status",
                Enum(&["running", "completed", "failed", "canceled"]),
            ),
            ("error", Json),
        ],
        "todo" => &[
            ("id", Id),
            ("revertToken?", Json),
            ("type", String),
            ("items", Array(todo)),
        ],
        "error" => &[
            ("id", Id),
            ("revertToken?", Json),
            ("type", String),
            ("message", String),
        ],
        "notification" => &[
            ("id", Id),
            ("revertToken?", Json),
            ("type", String),
            ("level", Enum(&["info", "warning", "error"])),
            ("message", String),
        ],
        "compaction" => &[
            ("id", Id),
            ("revertToken?", Json),
            ("type", String),
            ("status", Enum(&["loading", "completed"])),
            ("trigger?", Enum(&["auto", "manual"])),
            ("preTokens?", Number),
        ],
        "plugin" => &[
            ("id", Id),
            ("revertToken?", Json),
            ("type", String),
            ("pluginId", Id),
            ("kind", String),
            ("version", Number),
            ("data", Json),
        ],
        _ => bail!("Unknown provider timeline item type"),
    };
    object(value, fields, false)?;
    if value["type"] == "tool_call" && value["status"] != "failed" {
        field(&mut value["error"], Null)?;
    }
    Ok(())
}
fn permission_action(value: &mut Value) -> Result<()> {
    object(
        value,
        &[
            ("id", Id),
            ("label", String),
            ("behavior", Enum(&["allow", "deny"])),
            ("variant?", Enum(&["primary", "secondary", "danger"])),
            (
                "intent?",
                Enum(&["implement", "implement_resume", "dismiss"]),
            ),
        ],
        false,
    )
}
fn permission(value: &mut Value) -> Result<()> {
    object(
        value,
        &[
            ("id", Id),
            ("name", String),
            ("kind", Enum(&["tool", "plan", "question", "mode", "other"])),
            ("title?", String),
            ("description?", String),
            ("input?", Object),
            ("detail?", Nested(detail)),
            ("suggestions?", Array(json_object)),
            ("actions?", Array(permission_action)),
            ("metadata?", Object),
        ],
        false,
    )
}
fn prompt_result(value: &mut Value) -> Result<()> {
    match discriminator(value)? {
        "turn" | "steer" => object(value, &[("type", String), ("turnId", Id)], false),
        "completed" => object(value, &[("type", String)], false),
        "failed" => object(value, &[("type", String), ("error", Nested(error))], false),
        _ => bail!("Invalid provider prompt result"),
    }
}
pub(super) fn event(value: &mut Value) -> Result<()> {
    let fields: &[(&str, Field)] = match discriminator(value)? {
        "catalog" => &[
            ("type", String),
            ("requestId", Id),
            ("catalog", Nested(catalog)),
        ],
        "sessions" => &[
            ("type", String),
            ("requestId", Id),
            ("sessions", Array(summary)),
        ],
        "request.completed" => &[("type", String), ("requestId", Id)],
        "request.failed" => &[
            ("type", String),
            ("requestId", Id),
            ("error", Nested(error)),
        ],
        "session.opened" => &[
            ("type", String),
            ("requestId?", Id),
            ("sessionId", Id),
            ("parentSessionId?", Id),
            ("toolCallId?", Id),
            ("capabilities", Strings),
            ("restoration", Enum(&["core", "parent"])),
            ("persistence?", Nested(persistence)),
            ("title?", String),
            ("description?", String),
            ("cwd", String),
        ],
        "session.ready" => &[("type", String), ("requestId?", Id), ("sessionId", Id)],
        "session.closed" => &[
            ("type", String),
            ("sessionId", Id),
            ("error?", Nested(error)),
        ],
        "session.runtime_failed" => &[
            ("type", String),
            ("sessionId", Id),
            ("error", Nested(error)),
        ],
        "session.persistence" => &[
            ("type", String),
            ("sessionId", Id),
            ("persistence", Nested(persistence)),
        ],
        "session.prompt_result" => &[
            ("type", String),
            ("sessionId", Id),
            ("clientMessageId", Id),
            ("result", Nested(prompt_result)),
        ],
        "session.turn" => &[
            ("type", String),
            ("sessionId", Id),
            ("turnId", Id),
            (
                "state",
                Enum(&["started", "completed", "failed", "canceled"]),
            ),
            ("error?", Nested(error)),
        ],
        "session.usage" => &[
            ("type", String),
            ("sessionId", Id),
            ("turnId?", Id),
            ("usage", Nested(usage)),
        ],
        "session.config" => &[
            ("type", String),
            ("sessionId", Id),
            ("config", Nested(config)),
        ],
        "session.commands" => &[
            ("type", String),
            ("sessionId", Id),
            ("commands", Array(command)),
        ],
        "session.permission" => &[
            ("type", String),
            ("sessionId", Id),
            ("request", Nested(permission)),
        ],
        "session.permission_resolved" => {
            &[("type", String), ("sessionId", Id), ("permissionId", Id)]
        }
        "session.notice" => &[
            ("type", String),
            ("sessionId", Id),
            ("notice", Nested(notice)),
        ],
        "timeline.item" => &[
            ("type", String),
            ("sessionId", Id),
            ("item", Nested(timeline)),
            ("timestamp?", String),
        ],
        _ => bail!("Unknown provider event type"),
    };
    object(value, fields, false)
}

pub(super) fn envelope(value: &mut Value) -> Result<()> {
    let fields: &[(&str, Field)] = match discriminator(value)? {
        "provider.connected" => &[
            ("type", String),
            ("connectionId", Id),
            ("version", Positive),
            ("capabilities", Strings),
        ],
        "provider.connect_failed" => &[("type", String), ("connectionId", Id), ("error", String)],
        "provider.accepted" => &[("type", String), ("connectionId", Id), ("acceptanceId", Id)],
        "provider.rejected" => &[
            ("type", String),
            ("connectionId", Id),
            ("acceptanceId", Id),
            ("error", String),
        ],
        "provider.event" => &[
            ("type", String),
            ("connectionId", Id),
            ("event", Nested(event)),
        ],
        "provider.closed" => &[("type", String), ("connectionId", Id), ("error?", String)],
        _ => bail!("Unknown provider envelope type"),
    };
    object(value, fields, true)
}
