use anyhow::{Context, Result, bail, ensure};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::Path;
use tokio::io::AsyncWriteExt;
use url::Url;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Relationship {
    pub daemon_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
    pub hub_origin: String,
    pub created_at: String,
    pub permissions: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Credential {
    pub secret: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Enrollment {
    pub token: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Identity {
    pub server_id: String,
    pub daemon_public_key: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Transport {
    pub kind: String,
    pub web_socket_url: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Record {
    pub version: u8,
    pub state: String,
    pub relationship: Relationship,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credential: Option<Credential>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enrollment: Option<Enrollment>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identity: Option<Identity>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transport: Option<Transport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}
impl Record {
    pub fn secret(&self) -> Result<&str> {
        Ok(&self
            .credential
            .as_ref()
            .context("Missing Hub credential")?
            .secret)
    }
    pub fn principal_id(&self) -> Result<String> {
        Ok(format!(
            "hub:{}:{}",
            self.relationship.daemon_id,
            verifier(self.secret()?)
        ))
    }
    fn validate(&mut self) -> Result<()> {
        ensure!(self.version == 2, "Invalid Hub relationship version");
        ensure!(
            !self.relationship.daemon_id.is_empty(),
            "Missing Hub daemon ID"
        );
        normalize_url(&self.relationship.hub_origin)?;
        self.relationship.permissions =
            permissions(&serde_json::to_value(&self.relationship.permissions)?)?;
        match self.state.as_str() {
            "pending" => {
                let enrollment = self.enrollment.as_ref().context("Missing Hub enrollment")?;
                let identity = self.identity.as_ref().context("Missing Hub identity")?;
                ensure!(
                    !enrollment.token.is_empty()
                        && !identity.server_id.is_empty()
                        && !identity.daemon_public_key.is_empty(),
                    "Invalid pending Hub enrollment"
                );
            }
            "active" => {
                ensure!(self.transport.is_some(), "Missing Hub transport");
            }
            "disconnecting" | "revoked" => {}
            _ => bail!("Invalid Hub relationship state"),
        }
        if self.state != "revoked" {
            ensure!(!self.secret()?.is_empty(), "Missing Hub secret");
            ensure!(
                self.relationship
                    .idempotency_key
                    .as_ref()
                    .is_some_and(|s| !s.is_empty()),
                "Missing Hub enrollment key"
            );
        } else {
            self.credential = None;
            self.enrollment = None;
            self.identity = None;
            self.relationship.idempotency_key = None;
        }
        if let Some(transport) = &self.transport {
            ensure!(
                transport.kind == "direct_websocket",
                "Invalid Hub transport"
            );
            validate_socket_url(&self.relationship.hub_origin, &transport.web_socket_url)?;
        }
        Ok(())
    }
}
pub(super) fn verifier(secret: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(secret.as_bytes()))
}
pub(super) fn permissions(value: &Value) -> Result<Vec<String>> {
    let mut result = Vec::new();
    for permission in value.as_array().context("Invalid daemon permission")? {
        let p = permission.as_str().context("Invalid daemon permission")?;
        ensure!(
            matches!(
                p,
                "daemon.read"
                    | "daemon.manage"
                    | "tunnel.manage"
                    | "access.manage"
                    | "workspace.read"
                    | "workspace.write"
                    | "workspace.manage"
                    | "automation.manage"
                    | "hub.execute"
            ),
            "Invalid daemon permission"
        );
        if !result.iter().any(|v| v == p) {
            result.push(p.to_owned());
        }
    }
    Ok(result)
}
pub(super) fn same_permissions(actual: &Value, expected: &[String]) -> bool {
    actual.as_array().is_some_and(|a| {
        a.len() == expected.len()
            && expected
                .iter()
                .all(|v| a.iter().any(|x| x.as_str() == Some(v)))
    })
}
pub(super) fn normalize_url(value: &str) -> Result<String> {
    let mut url = Url::parse(value)?;
    ensure!(
        matches!(url.scheme(), "http" | "https"),
        "Hub URL must use HTTP or HTTPS"
    );
    ensure!(
        url.username().is_empty() && url.password().is_none(),
        "Hub URL cannot include credentials"
    );
    ensure!(
        url.query().is_none() && url.fragment().is_none(),
        "Hub URL cannot include a query or fragment"
    );
    let path = url
        .path()
        .strip_suffix('/')
        .unwrap_or(url.path())
        .to_owned();
    url.set_path(&path);
    Ok(url
        .as_str()
        .strip_suffix('/')
        .unwrap_or(url.as_str())
        .to_owned())
}
pub(super) fn validate_socket_url(origin: &str, value: &str) -> Result<()> {
    let origin = Url::parse(origin)?;
    let socket = Url::parse(value)?;
    ensure!(
        socket.fragment().is_none(),
        "Hub WebSocket URL cannot include a fragment"
    );
    let expected = if origin.scheme() == "https" {
        "wss"
    } else {
        "ws"
    };
    ensure!(
        socket.scheme() == expected
            && socket.host_str() == origin.host_str()
            && socket.port_or_known_default() == origin.port_or_known_default(),
        "Hub WebSocket URL must match the Hub origin"
    );
    Ok(())
}
pub(super) async fn persist(path: &Path, record: &Record) -> Result<()> {
    let mut bytes = serde_json::to_vec_pretty(record)?;
    bytes.push(b'\n');
    let temp = path.with_file_name(format!(".hub-relationship-{}.tmp", uuid::Uuid::new_v4()));
    let result: Result<()> = async {
        let mut options = tokio::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = options.open(&temp).await?;
        file.write_all(&bytes).await?;
        file.sync_all().await?;
        drop(file);
        tokio::fs::rename(&temp, path).await?;
        Ok(())
    }
    .await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(&temp).await;
    }
    result
}
pub(super) async fn remove(path: &Path) -> Result<()> {
    match tokio::fs::remove_file(path).await {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}
pub(super) async fn load(path: &Path) -> Result<Option<Record>> {
    let bytes = match tokio::fs::read(path).await {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let parsed = (|| -> Result<(Record, bool)> {
        let mut value: Value = serde_json::from_slice(&bytes)?;
        let legacy = value["version"] == 1;
        if legacy {
            let scopes = value["relationship"]["scopes"]
                .as_array()
                .context("Invalid legacy Hub scopes")?;
            ensure!(
                scopes.iter().all(Value::is_string),
                "Invalid legacy Hub scopes"
            );
            let grants = if scopes.iter().any(|v| v == "hub.execution.*") {
                serde_json::json!(["hub.execute"])
            } else {
                serde_json::json!([])
            };
            value["version"] = 2.into();
            value["relationship"]
                .as_object_mut()
                .context("Invalid Hub relationship")?
                .remove("scopes");
            value["relationship"]["permissions"] = grants;
        }
        let mut record: Record = serde_json::from_value(value)?;
        record.validate()?;
        Ok((record, legacy))
    })();
    let (record, legacy) = match parsed {
        Ok(result) => result,
        Err(error) => {
            let quarantine = path.with_file_name(format!(
                "hub-relationship.invalid-{}-{}.json",
                chrono::Utc::now().timestamp_millis(),
                uuid::Uuid::new_v4()
            ));
            tokio::fs::rename(path, &quarantine).await?;
            private(&quarantine).await?;
            tracing::error!(%error, path=%quarantine.display(), "Quarantined invalid Hub relationship authority");
            return Ok(None);
        }
    };
    if legacy {
        persist(path, &record).await?;
    }
    private(path).await?;
    if record.state == "disconnecting" {
        remove(path).await?;
        return Ok(None);
    }
    Ok(Some(record))
}
async fn private(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).await?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}
