use super::record::{Record, same_permissions, validate_socket_url, verifier};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::net::TcpStream;
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream,
    tungstenite::{self, client::IntoClientRequest},
};

#[derive(Debug, thiserror::Error)]
#[error("Hub enrollment failed ({0})")]
pub(super) struct EnrollmentRejected(pub u16);
#[derive(Debug, thiserror::Error)]
#[error("Hub rejected socket authentication ({0})")]
pub(super) struct SocketRejected(pub u16);
pub(super) type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;
pub(super) struct Remote {
    http: reqwest::Client,
}
impl Remote {
    pub fn new() -> Result<Self> {
        Ok(Self {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(15))
                .build()?,
        })
    }
    pub async fn enroll(&self, record: &Record) -> Result<String> {
        let relationship = &record.relationship;
        let identity = record
            .identity
            .as_ref()
            .context("Missing Hub enrollment identity")?;
        let response = self.http.post(format!("{}/api/daemons/enroll",relationship.hub_origin))
            .bearer_auth(&record.enrollment.as_ref().context("Missing enrollment token")?.token)
            .json(&json!({"daemonId":relationship.daemon_id,"idempotencyKey":relationship.idempotency_key,
                "hostname":hostname::get()?.to_string_lossy(),"serverId":identity.server_id,"daemonPublicKey":identity.daemon_public_key,
                "credentialVerifier":verifier(record.secret()?),"permissions":relationship.permissions}))
            .send().await?;
        let status = response.status();
        if matches!(status.as_u16(), 401 | 403) {
            return Err(EnrollmentRejected(status.as_u16()).into());
        }
        ensure!(
            status.is_success(),
            "Hub enrollment failed ({})",
            status.as_u16()
        );
        let value: Value = response.json().await?;
        ensure!(
            value["daemonId"].as_str() == Some(&relationship.daemon_id)
                && same_permissions(&value["permissions"], &relationship.permissions),
            "Hub enrollment response did not match the pending relationship"
        );
        let socket = value["webSocketUrl"]
            .as_str()
            .context("Invalid Hub WebSocket URL")?;
        validate_socket_url(&relationship.hub_origin, socket)?;
        Ok(socket.to_owned())
    }
    pub async fn update(&self, record: &Record, permissions: &[String]) -> Result<()> {
        let response = self
            .http
            .patch(relationship_url(record)?)
            .bearer_auth(record.secret()?)
            .json(&json!({"permissions":permissions}))
            .send()
            .await?;
        ensure!(
            response.status().is_success(),
            "Hub permission update failed ({})",
            response.status().as_u16()
        );
        let value: Value = response.json().await?;
        ensure!(
            same_permissions(&value["permissions"], permissions),
            "Hub permission response did not match the local grant"
        );
        Ok(())
    }
    pub async fn revoke(&self, record: &Record) -> Result<()> {
        let response = self
            .http
            .delete(relationship_url(record)?)
            .bearer_auth(record.secret()?)
            .send()
            .await?;
        ensure!(
            response.status().is_success() || matches!(response.status().as_u16(), 401 | 403 | 404),
            "Hub revocation failed ({})",
            response.status().as_u16()
        );
        Ok(())
    }
    pub async fn open(&self, record: &Record) -> Result<(Socket, bool)> {
        let transport = record
            .transport
            .as_ref()
            .context("Missing Hub socket transport")?;
        validate_socket_url(&record.relationship.hub_origin, &transport.web_socket_url)?;
        let mut request = transport.web_socket_url.as_str().into_client_request()?;
        request.headers_mut().insert(
            "authorization",
            format!("Bearer {}", record.secret()?).parse()?,
        );
        request
            .headers_mut()
            .insert("x-paseo-daemon-id", record.relationship.daemon_id.parse()?);
        request
            .headers_mut()
            .insert("x-paseo-session-protocol", "1".parse()?);
        let config = tungstenite::protocol::WebSocketConfig::default()
            .max_message_size(Some(100 * 1024 * 1024))
            .max_frame_size(Some(100 * 1024 * 1024));
        let opened = tokio::time::timeout(
            Duration::from_secs(15),
            tokio_tungstenite::connect_async_with_config(request, Some(config), false),
        )
        .await
        .context("Hub WebSocket handshake timed out")?;
        match opened {
            Ok((socket, response)) => {
                let session_v1 = response
                    .headers()
                    .get("x-paseo-session-protocol")
                    .is_some_and(|v| v == "1");
                Ok((socket, session_v1))
            }
            Err(tungstenite::Error::Http(response))
                if matches!(response.status().as_u16(), 401 | 403) =>
            {
                Err(SocketRejected(response.status().as_u16()).into())
            }
            Err(error) => bail!(error),
        }
    }
}
fn relationship_url(record: &Record) -> Result<url::Url> {
    let mut url = url::Url::parse(&format!("{}/api/daemons/", record.relationship.hub_origin))?;
    url.path_segments_mut()
        .map_err(|_| anyhow::anyhow!("Invalid Hub origin"))?
        .pop_if_empty()
        .push(&record.relationship.daemon_id);
    Ok(url)
}
