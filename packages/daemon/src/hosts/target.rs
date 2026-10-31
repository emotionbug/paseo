use super::HostError;
use crate::relay_crypto::SecurityMode;
use anyhow::{Result, ensure};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::Value;
use url::Url;

#[derive(Clone)]
pub(crate) enum Endpoint {
    Socket {
        url: String,
        ipc: Option<String>,
    },
    Ssh {
        host: String,
        port: Option<u16>,
        daemon_port: u16,
    },
}
#[derive(Clone)]
pub(crate) struct Target {
    pub endpoint: Endpoint,
    pub password: Option<String>,
    pub security: Option<(String, SecurityMode)>,
    pub description: String,
}
impl Target {
    pub fn parse(input: &str) -> Result<Self> {
        Self::parse_inner(input.trim()).map_err(|_| HostError::new("invalid_connection", "Invalid host connection. Use host:port, ws(s)://, tcp://, unix://, pipe://, ssh:// or a pairing URL.").into())
    }
    fn parse_inner(input: &str) -> Result<Self> {
        ensure!(!input.is_empty(), "empty connection");
        if input.starts_with("relay://") || input.contains("#connect=") {
            let decoded;
            let text = if let Some((_, fragment)) = input.split_once("#connect=") {
                decoded = percent_encoding::percent_decode_str(fragment)
                    .decode_utf8()?
                    .into_owned();
                &decoded
            } else {
                input
            };
            let url = Url::parse(text)?;
            ensure!(
                url.scheme() == "relay" && url.username().is_empty() && url.password().is_none(),
                "invalid relay"
            );
            let server = percent_encoding::percent_decode_str(url.path().trim_start_matches('/'))
                .decode_utf8()?;
            ensure!(
                !server.is_empty() && !server.contains('/'),
                "invalid server id"
            );
            let endpoint = authority(&url, true)?;
            let key = query(&url, "key").ok_or_else(|| anyhow::anyhow!("missing key"))?;
            let security = query(&url, "security");
            ensure!(
                security.as_deref().is_none_or(|s| s == "protected"),
                "invalid security"
            );
            return Self::relay(
                &endpoint,
                &server,
                &key,
                query(&url, "ssl").as_deref() == Some("true"),
                security.is_some(),
                query(&url, "password").filter(|s| !s.is_empty()),
            );
        }
        if let Some((_, fragment)) = input.split_once("#offer=") {
            let offer: Value = serde_json::from_slice(
                &URL_SAFE_NO_PAD.decode(fragment.trim().trim_end_matches('='))?,
            )?;
            ensure!(offer["v"] == 2, "invalid offer");
            let endpoint = required(&offer["relay"], "endpoint")?;
            let endpoint_url = valid_endpoint(endpoint)?;
            let protected = match offer.get("minimumSecurityMode") {
                None => false,
                Some(v) if v == "protected" => true,
                _ => anyhow::bail!("invalid security"),
            };
            return Self::relay(
                endpoint,
                required(&offer, "serverId")?,
                required(&offer, "daemonPublicKeyB64")?,
                offer["relay"]["useTls"]
                    .as_bool()
                    .unwrap_or(endpoint_url.port() == Some(443)),
                protected,
                None,
            );
        }
        if input.starts_with("unix://")
            || input.starts_with("pipe://")
            || input.starts_with('/')
            || input.starts_with('~')
            || input.starts_with("\\\\.\\pipe\\")
        {
            let pipe = input.starts_with("pipe://") || input.starts_with("\\\\.\\pipe\\");
            let path = input
                .strip_prefix("unix://")
                .or_else(|| input.strip_prefix("pipe://"))
                .unwrap_or(input);
            ensure!(!path.is_empty() && path != "~", "empty socket");
            return Ok(Self {
                endpoint: Endpoint::Socket {
                    url: "ws://localhost/ws".into(),
                    ipc: Some(path.into()),
                },
                password: None,
                security: None,
                description: format!("{}://{path}", if pipe { "pipe" } else { "unix" }),
            });
        }
        if input.starts_with("ssh://") {
            let url = Url::parse(input)?;
            ensure!(
                url.password().is_none()
                    && matches!(url.path(), "" | "/")
                    && url.fragment().is_none(),
                "invalid ssh"
            );
            let pairs: Vec<_> = url.query_pairs().collect();
            ensure!(
                pairs
                    .iter()
                    .all(|(k, _)| k == "daemonPort" || k == "password"),
                "unknown ssh option"
            );
            ensure!(
                ["daemonPort", "password"].iter().all(|key| pairs
                    .iter()
                    .filter(|(k, _)| k.as_ref() == *key)
                    .count()
                    <= 1),
                "duplicate ssh option"
            );
            let hostname = url
                .host_str()
                .ok_or_else(|| anyhow::anyhow!("missing host"))?;
            let username = percent_encoding::percent_decode_str(url.username()).decode_utf8()?;
            let bare = hostname.trim_start_matches('[').trim_end_matches(']');
            let host = if username.is_empty() {
                bare.to_owned()
            } else {
                format!("{username}@{bare}")
            };
            ensure!(
                !host.starts_with('-') && !host.chars().any(char::is_whitespace),
                "invalid ssh host"
            );
            let daemon_port = query(&url, "daemonPort")
                .map(|s| parse_daemon_port(&s))
                .transpose()?
                .unwrap_or(6767);
            ensure!(daemon_port != 0 && url.port() != Some(0), "invalid port");
            return Ok(Self {
                endpoint: Endpoint::Ssh {
                    host,
                    port: url.port(),
                    daemon_port,
                },
                password: query(&url, "password").filter(|s| !s.is_empty()),
                security: None,
                description: format!(
                    "ssh://{hostname}{} (daemon port {daemon_port})",
                    url.port().map(|p| format!(":{p}")).unwrap_or_default()
                ),
            });
        }
        if input.starts_with("ws://") || input.starts_with("wss://") {
            let mut url = Url::parse(input)?;
            ensure!(
                url.host_str().is_some()
                    && url.username().is_empty()
                    && url.password().is_none()
                    && url.fragment().is_none(),
                "invalid websocket"
            );
            let password = query(&url, "password");
            let pairs: Vec<(String, String)> = url
                .query_pairs()
                .filter(|(k, _)| k != "password")
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect();
            url.set_query(None);
            if !pairs.is_empty() {
                url.query_pairs_mut().extend_pairs(pairs);
            }
            if url.path() == "/" {
                url.set_path("/ws");
            }
            return Ok(Self {
                description: url.origin().ascii_serialization(),
                endpoint: Endpoint::Socket {
                    url: url.into(),
                    ipc: None,
                },
                password,
                security: None,
            });
        }
        let (endpoint, tls, password) = if input.starts_with("tcp://") {
            let url = Url::parse(input)?;
            ensure!(
                matches!(url.path(), "" | "/")
                    && url.fragment().is_none()
                    && url.username().is_empty()
                    && url.password().is_none(),
                "invalid tcp"
            );
            (
                authority(&url, true)?,
                query(&url, "ssl").as_deref() == Some("true"),
                query(&url, "password").filter(|s| !s.is_empty()),
            )
        } else {
            (
                if input.bytes().all(|b| b.is_ascii_digit()) {
                    format!("127.0.0.1:{input}")
                } else {
                    input.into()
                },
                false,
                None,
            )
        };
        valid_endpoint(&endpoint)?;
        Ok(Self {
            endpoint: Endpoint::Socket {
                url: format!("{}://{endpoint}/ws", if tls { "wss" } else { "ws" }),
                ipc: None,
            },
            password,
            security: None,
            description: format!("tcp://{endpoint}"),
        })
    }
    fn relay(
        endpoint: &str,
        server: &str,
        key: &str,
        tls: bool,
        protected: bool,
        password: Option<String>,
    ) -> Result<Self> {
        valid_endpoint(endpoint)?;
        let mut url = Url::parse(&format!(
            "{}://{endpoint}/ws",
            if tls { "wss" } else { "ws" }
        ))?;
        url.query_pairs_mut()
            .append_pair("serverId", server)
            .append_pair("role", "client")
            .append_pair("v", "2");
        let encoded: String = url::form_urlencoded::byte_serialize(server.as_bytes()).collect();
        ensure!(!key.is_empty(), "missing key");
        Ok(Self {
            endpoint: Endpoint::Socket {
                url: url.into(),
                ipc: None,
            },
            password,
            security: Some((
                key.into(),
                if protected {
                    SecurityMode::Protected
                } else {
                    SecurityMode::Legacy
                },
            )),
            description: format!("relay://{endpoint}/{encoded}"),
        })
    }
}
fn query(url: &Url, key: &str) -> Option<String> {
    url.query_pairs()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.into_owned())
}
fn required<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("missing field"))
}
fn authority(url: &Url, required_port: bool) -> Result<String> {
    let host = url
        .host_str()
        .ok_or_else(|| anyhow::anyhow!("missing host"))?;
    let port = url
        .port()
        .or_else(|| {
            if required_port {
                None
            } else {
                url.port_or_known_default()
            }
        })
        .ok_or_else(|| anyhow::anyhow!("missing port"))?;
    ensure!(port > 0, "invalid port");
    Ok(format!("{host}:{port}"))
}
fn valid_endpoint(input: &str) -> Result<Url> {
    let (host, port) = input
        .rsplit_once(':')
        .ok_or_else(|| anyhow::anyhow!("missing port"))?;
    ensure!(
        !port.is_empty()
            && port.len() <= 5
            && port.bytes().all(|b| b.is_ascii_digit())
            && port.parse::<u16>()? > 0,
        "invalid port"
    );
    if host.starts_with('[') {
        ensure!(
            host.ends_with(']')
                && host[1..host.len() - 1]
                    .parse::<std::net::Ipv6Addr>()
                    .is_ok(),
            "invalid ipv6"
        );
    } else {
        ensure!(
            !host.is_empty()
                && host
                    .bytes()
                    .enumerate()
                    .all(|(i, b)| b.is_ascii_alphanumeric()
                        || b == b'_'
                        || (i > 0 && matches!(b, b'.' | b'-'))),
            "invalid host"
        );
    }
    Ok(Url::parse(&format!("http://{input}"))?)
}
pub(super) fn ssh_args(host: &str, port: Option<u16>, daemon_port: u16) -> Vec<String> {
    let mut args = vec![
        "-T",
        "-o",
        "BatchMode=yes",
        "-o",
        "ConnectTimeout=10",
        "-o",
        "ClearAllForwardings=yes",
        "-o",
        "ExitOnForwardFailure=yes",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    if let Some(port) = port {
        args.extend(["-p".into(), port.to_string()]);
    }
    args.extend(["-W".into(), format!("127.0.0.1:{daemon_port}"), host.into()]);
    args
}
fn parse_daemon_port(value: &str) -> Result<u16> {
    let value = value.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}');
    let number = if let Some(digits) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
    {
        u64::from_str_radix(digits, 16)? as f64
    } else if let Some(digits) = value
        .strip_prefix("0b")
        .or_else(|| value.strip_prefix("0B"))
    {
        u64::from_str_radix(digits, 2)? as f64
    } else if let Some(digits) = value
        .strip_prefix("0o")
        .or_else(|| value.strip_prefix("0O"))
    {
        u64::from_str_radix(digits, 8)? as f64
    } else {
        value.parse::<f64>()?
    };
    ensure!(
        number.is_finite() && number.fract() == 0.0 && (1.0..=65535.0).contains(&number),
        "invalid daemon port"
    );
    Ok(number as u16)
}
