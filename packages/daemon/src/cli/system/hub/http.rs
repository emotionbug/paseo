use super::super::error;
use anyhow::Result;
use serde_json::{Value, json};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

pub(super) struct HubHttp {
    client: reqwest::Client,
    cancel: CancellationToken,
}
impl HubHttp {
    pub fn new(cancel: CancellationToken) -> Result<Self> {
        Ok(Self {
            client: reqwest::Client::builder().build()?,
            cancel,
        })
    }
    pub async fn request(
        &self,
        origin: &str,
        path: &str,
        key: Option<&str>,
        body: Option<&Value>,
        expected: u16,
        timeout: Duration,
    ) -> Result<Value> {
        let mut request = self
            .client
            .request(
                if body.is_some() {
                    reqwest::Method::POST
                } else {
                    reqwest::Method::GET
                },
                format!("{origin}{path}"),
            )
            .timeout(timeout);
        if let Some(key) = key {
            let mut header = reqwest::header::HeaderValue::from_str(&format!("Bearer {key}"))
                .map_err(|_| error("HUB_API_KEY_REQUIRED", "Hub credential is invalid"))?;
            header.set_sensitive(true);
            request = request.header(reqwest::header::AUTHORIZATION, header);
        }
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = tokio::select! {_=self.cancel.cancelled()=>return Err(error("CANCELLED","Hub operation cancelled")),response=request.send()=>response.map_err(|_|error("HUB_NETWORK_ERROR",format!("Could not reach Paseo Hub at {origin}. Check the Hub URL and network connection.")))?};
        let status = response.status().as_u16();
        if status != expected {
            let is_problem = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| v.to_lowercase().contains("application/problem+json"));
            if !is_problem {
                return Err(error(
                    "HUB_REQUEST_FAILED",
                    format!("Hub request failed with HTTP {status}."),
                ));
            }
            let problem: Value = response.json().await.map_err(|_| invalid())?;
            let mut message = problem_message(&problem, status)?;
            for secret in key
                .into_iter()
                .chain(body.and_then(|v| v["deviceCode"].as_str()))
            {
                if !secret.is_empty() {
                    message = message.replace(secret, "[redacted]");
                }
            }
            return Err(error(
                match status {
                    422 => "HUB_VALIDATION_FAILED",
                    404 => "HUB_NOT_FOUND",
                    _ => "HUB_REQUEST_FAILED",
                },
                message,
            ));
        }
        let value: Value = tokio::select! {_=self.cancel.cancelled()=>return Err(error("CANCELLED","Hub operation cancelled")),value=response.json()=>value.map_err(|_|invalid())?};
        if !valid(path, &value) {
            return Err(invalid());
        }
        Ok(value)
    }
    pub async fn get(&self, origin: &str, path: &str, key: &str) -> Result<Value> {
        self.request(origin, path, Some(key), None, 200, Duration::from_secs(15))
            .await
    }
    pub async fn post(
        &self,
        origin: &str,
        path: &str,
        key: Option<&str>,
        body: &Value,
        status: u16,
    ) -> Result<Value> {
        self.request(
            origin,
            path,
            key,
            Some(body),
            status,
            Duration::from_secs(15),
        )
        .await
    }
    pub async fn authorize(&self, origin: &str) -> Result<String> {
        let authorization = self
            .post(origin, "/api/v1/cli-authorizations", None, &json!({}), 201)
            .await?;
        eprintln!(
            "Open {} and enter code {}",
            authorization["verificationUri"].as_str().unwrap(),
            authorization["userCode"].as_str().unwrap()
        );
        use std::io::IsTerminal;
        if std::io::stderr().is_terminal() {
            // Browser launch is advisory; printed instructions remain usable
            // when no desktop URL handler is installed.
            let _ = open_browser(authorization["verificationUriComplete"].as_str().unwrap());
        }
        let expires =
            chrono::DateTime::parse_from_rfc3339(authorization["expiresAt"].as_str().unwrap())
                .map_err(|_| invalid())?
                .with_timezone(&chrono::Utc);
        let mut interval = authorization["interval"].as_u64().unwrap();
        loop {
            let remaining = (expires - chrono::Utc::now())
                .to_std()
                .map_err(|_| error("HUB_LOGIN_EXPIRED", "Hub CLI login expired"))?;
            tokio::select! {_=self.cancel.cancelled()=>return Err(error("CANCELLED","Hub login cancelled")),_=tokio::time::sleep(Duration::from_secs(interval).min(remaining))=>{}}
            let lifetime = (expires - chrono::Utc::now())
                .to_std()
                .map_err(|_| error("HUB_LOGIN_EXPIRED", "Hub CLI login expired"))?;
            let outcome = self
                .request(
                    origin,
                    "/api/v1/cli-authorizations/poll",
                    None,
                    Some(&json!({"deviceCode":authorization["deviceCode"]})),
                    200,
                    lifetime,
                )
                .await;
            let outcome = match outcome {
                Err(e)
                    if e.downcast_ref::<crate::client::ClientError>()
                        .is_some_and(|e| e.code == "HUB_NETWORK_ERROR") =>
                {
                    continue;
                }
                other => other?,
            };
            match outcome["status"].as_str().unwrap() {
                "authorized" => return Ok(outcome["credential"].as_str().unwrap().to_owned()),
                "denied" => return Err(error("HUB_LOGIN_DENIED", "Hub CLI login was denied")),
                "expired" => return Err(error("HUB_LOGIN_EXPIRED", "Hub CLI login expired")),
                "disclosed" => {
                    return Err(error(
                        "HUB_LOGIN_DISCLOSED",
                        "Hub CLI login was already completed",
                    ));
                }
                "retry_later" => {}
                _ => interval = outcome["interval"].as_u64().unwrap(),
            }
        }
    }
}
fn open_browser(url: &str) -> std::io::Result<()> {
    let mut command = tokio::process::Command::new(if cfg!(windows) {
        "rundll32.exe"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    });
    if cfg!(windows) {
        command.arg("url.dll,FileProtocolHandler");
    }
    command
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(unix)]
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    #[cfg(windows)]
    command.creation_flags(0x00000008 | 0x00000200);
    let mut child = command.spawn()?;
    tokio::spawn(async move {
        let _ = child.wait().await;
    });
    Ok(())
}
fn invalid() -> anyhow::Error {
    error("HUB_INVALID_RESPONSE", "Hub returned a malformed response.")
}
fn exact(value: &Value, keys: &[&str]) -> bool {
    value
        .as_object()
        .is_some_and(|o| o.len() == keys.len() && keys.iter().all(|k| o.contains_key(*k)))
}
fn text(value: &Value, key: &str, min: usize) -> bool {
    value[key]
        .as_str()
        .is_some_and(|s| s.encode_utf16().count() >= min)
}
fn uuid(value: &Value, key: &str) -> bool {
    value[key]
        .as_str()
        .is_some_and(|s| uuid::Uuid::parse_str(s).is_ok())
}
fn date(value: &Value, key: &str) -> bool {
    value[key]
        .as_str()
        .is_some_and(|s| s.ends_with('Z') && chrono::DateTime::parse_from_rfc3339(s).is_ok())
}
fn positive(value: &Value, key: &str) -> bool {
    value[key].as_u64().is_some_and(|n| n > 0)
}
fn activation(value: &Value, key: &str) -> bool {
    value[key]
        .as_str()
        .and_then(|s| url::Url::parse(s).ok())
        .is_some_and(|u| matches!(u.scheme(), "http" | "https"))
}
pub(super) fn trigger_name(value: &str) -> bool {
    value.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}
fn valid(path: &str, v: &Value) -> bool {
    match path {
        "/api/v1/cli-authorizations" => {
            exact(
                v,
                &[
                    "deviceCode",
                    "userCode",
                    "verificationUri",
                    "verificationUriComplete",
                    "expiresAt",
                    "interval",
                ],
            ) && text(v, "deviceCode", 32)
                && text(v, "userCode", 1)
                && activation(v, "verificationUri")
                && activation(v, "verificationUriComplete")
                && date(v, "expiresAt")
                && positive(v, "interval")
        }
        "/api/v1/cli-authorizations/poll" => match v["status"].as_str() {
            Some("retry_later") => exact(v, &["status"]),
            Some("authorized") => {
                exact(v, &["status", "interval", "credential", "organizationId"])
                    && positive(v, "interval")
                    && text(v, "credential", 32)
                    && text(v, "organizationId", 1)
            }
            Some("pending" | "slow_down" | "denied" | "expired" | "disclosed") => {
                exact(v, &["status", "interval"]) && positive(v, "interval")
            }
            _ => false,
        },
        "/api/v1/daemons/enrollment-tokens" => {
            exact(v, &["token", "expiresAt"]) && text(v, "token", 32) && date(v, "expiresAt")
        }
        "/api/v1/projects" => {
            exact(v, &["projects"])
                && v["projects"].as_array().is_some_and(|a| {
                    a.iter().all(|p| {
                        exact(p, &["id", "slug", "name"])
                            && uuid(p, "id")
                            && text(p, "slug", 1)
                            && text(p, "name", 1)
                    })
                })
        }
        "/api/v1/triggers" => {
            exact(v, &["triggers"])
                && v["triggers"].as_array().is_some_and(|a| {
                    a.iter().all(|p| {
                        exact(p, &["id", "name", "enabled", "format", "yaml"])
                            && uuid(p, "id")
                            && p["name"].as_str().is_some_and(trigger_name)
                            && p["enabled"].is_boolean()
                            && matches!(
                                p["format"].as_str(),
                                Some("single_run" | "legacy_multistep")
                            )
                            && p["yaml"].is_string()
                    })
                })
        }
        "/api/v1/triggers/validate" => {
            exact(v, &["name", "valid"])
                && v["name"].as_str().is_some_and(trigger_name)
                && v["valid"] == true
        }
        "/api/v1/triggers/install" => {
            exact(v, &["triggerId", "name", "revisionId", "version", "active"])
                && uuid(v, "triggerId")
                && v["name"].as_str().is_some_and(trigger_name)
                && uuid(v, "revisionId")
                && positive(v, "version")
                && v["active"] == true
        }
        "/api/v1/configurations/validate" => {
            exact(v, &["projectSlug", "valid"]) && text(v, "projectSlug", 1) && v["valid"] == true
        }
        "/api/v1/configurations/install" => {
            exact(v, &["projectSlug", "version", "versionId", "active"])
                && text(v, "projectSlug", 1)
                && positive(v, "version")
                && uuid(v, "versionId")
                && v["active"] == true
        }
        "/api/v1/configuration-resources" => {
            exact(v, &["daemons", "github", "discord", "slack", "linear"])
                && v["daemons"].as_array().is_some_and(|a| {
                    a.iter()
                        .all(|p| exact(p, &["id", "slug"]) && uuid(p, "id") && text(p, "slug", 1))
                })
                && v["github"].as_array().is_some_and(|a| {
                    a.iter().all(|p| {
                        exact(p, &["slug", "accountLogin", "accountType", "repositories"])
                            && text(p, "slug", 1)
                            && text(p, "accountLogin", 1)
                            && text(p, "accountType", 1)
                            && p["repositories"].as_array().is_some_and(|r| {
                                r.iter().all(|s| s.as_str().is_some_and(|s| !s.is_empty()))
                            })
                    })
                })
                && [
                    ("discord", "guildName"),
                    ("slack", "teamName"),
                    ("linear", "organizationName"),
                ]
                .iter()
                .all(|(kind, name)| {
                    v[*kind].as_array().is_some_and(|a| {
                        a.iter().all(|p| {
                            exact(p, &["slug", *name]) && text(p, "slug", 1) && text(p, name, 1)
                        })
                    })
                })
        }
        _ => false,
    }
}
fn problem_message(value: &Value, status: u16) -> Result<String> {
    let object = value.as_object().ok_or_else(invalid)?;
    for key in ["type", "title", "detail", "instance"] {
        if object.get(key).is_some_and(|v| !v.is_string()) {
            return Err(invalid());
        }
    }
    if object
        .get("status")
        .is_some_and(|v| v.as_u64() != Some(status as u64))
    {
        return Err(invalid());
    }
    let mut lines = Vec::new();
    let fields = if value["errors"].is_array() {
        Some(&value["errors"])
    } else {
        object.get("issues")
    };
    if let Some(fields) = fields {
        for field in fields.as_array().ok_or_else(invalid)? {
            let message = field["message"].as_str().ok_or_else(invalid)?;
            let name = if let Some(name) = field.get("field") {
                Some(name.as_str().ok_or_else(invalid)?.to_owned())
            } else {
                match field.get("path") {
                    None => None,
                    Some(Value::String(s)) => Some(s.clone()),
                    Some(Value::Array(parts)) => {
                        let mut path = String::new();
                        for (i, part) in parts.iter().enumerate() {
                            if let Some(number) = part.as_u64() {
                                path.push_str(&format!("[{number}]"));
                            } else if let Some(text) = part.as_str() {
                                if !path.is_empty() {
                                    path.push_str(
                                        if i == 1
                                            && parts[0]
                                                .as_str()
                                                .is_some_and(|s| s.starts_with(".paseo/"))
                                        {
                                            ": "
                                        } else {
                                            "."
                                        },
                                    );
                                }
                                path.push_str(text);
                            } else {
                                return Err(invalid());
                            }
                        }
                        Some(path)
                    }
                    _ => return Err(invalid()),
                }
            };
            lines.push(
                name.map(|name| format!("{name}: {message}"))
                    .unwrap_or(message.to_owned()),
            );
        }
    } else if let Some(errors) = object.get("errors") {
        for (field, messages) in errors.as_object().ok_or_else(invalid)? {
            for message in messages.as_array().ok_or_else(invalid)? {
                lines.push(format!(
                    "{field}: {}",
                    message.as_str().ok_or_else(invalid)?
                ));
            }
        }
    }
    let title = value["title"]
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| format!("Hub request failed with HTTP {status}"));
    Ok(if !lines.is_empty() {
        format!("{title}\n{}", lines.join("\n"))
    } else if let Some(detail) = value["detail"].as_str() {
        format!("{title}: {detail}")
    } else {
        title
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_traversal_trigger_names_and_extra_response_fields() {
        assert!(!valid(
            "/api/v1/triggers",
            &json!({"triggers":[{"id":uuid::Uuid::new_v4(),"name":"../leak","enabled":true,"format":"single_run","yaml":""}]})
        ));
        assert!(!valid(
            "/api/v1/triggers/validate",
            &json!({"name":"ok","valid":true,"secret":"hidden"})
        ));
    }
}
