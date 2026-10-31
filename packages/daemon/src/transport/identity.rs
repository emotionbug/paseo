use super::storage;
use crate::relay_crypto::KeyPair;
use anyhow::{Result, anyhow, bail};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::RngCore;
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::Path, sync::Arc};

pub(super) async fn load(
    home: &Path,
    environment: &BTreeMap<String, String>,
) -> Result<(Value, Arc<KeyPair>)> {
    let server_path = home.join("server-id");
    let stored = storage::read_private(&server_path).await?;
    let override_id = environment
        .get("PASEO_SERVER_ID")
        .map(|s| s.trim())
        .filter(|s| !s.is_empty());
    let server_id = if let Some(id) = override_id {
        if stored.is_none() {
            storage::write_private(&server_path, format!("{id}\n").as_bytes()).await?;
        }
        id.to_owned()
    } else if let Some(bytes) = stored {
        let id = String::from_utf8(bytes)?.trim().to_owned();
        if id.is_empty() {
            bail!("Persisted server identity is empty");
        }
        id
    } else {
        let mut bytes = [0u8; 9];
        rand::rngs::OsRng.fill_bytes(&mut bytes);
        let id = format!("srv_{}", URL_SAFE_NO_PAD.encode(bytes));
        storage::write_private(&server_path, format!("{id}\n").as_bytes()).await?;
        id
    };
    let key_path = home.join("daemon-keypair.json");
    let key = match storage::read_private(&key_path).await? {
        Some(bytes) => {
            let value: Value = serde_json::from_slice(&bytes)?;
            if value["v"] != 2 {
                bail!("Unsupported daemon keypair version");
            }
            KeyPair::from_base64(
                value["publicKeyB64"]
                    .as_str()
                    .ok_or_else(|| anyhow!("Missing public key"))?,
                value["secretKeyB64"]
                    .as_str()
                    .ok_or_else(|| anyhow!("Missing secret key"))?,
            )?
        }
        None => {
            let key = KeyPair::generate()?;
            let secret = key.export_secret_key();
            let mut bytes = serde_json::to_vec_pretty(
                &json!({"v":2,"publicKeyB64":key.export_public_key(),"secretKeyB64":secret.as_str()}),
            )?;
            bytes.push(b'\n');
            let result = storage::write_private(&key_path, &bytes).await;
            use zeroize::Zeroize;
            bytes.zeroize();
            result?;
            key
        }
    };
    let identity = json!({"serverId":server_id,"daemonPublicKey":key.export_public_key(),"label":hostname::get()?.to_string_lossy()});
    Ok((identity, Arc::new(key)))
}

pub async fn generate_local_pairing_offer(
    home: &Path,
    config: &Value,
    include_qr: bool,
) -> Result<Value> {
    if config["relayEnabled"].as_bool() == Some(false) {
        return Ok(json!({"relayEnabled":false,"url":null,"qr":null}));
    }
    let (identity, _) = load(home, &BTreeMap::new()).await?;
    pairing_offer(&identity, config, include_qr)
}

pub async fn offline_pairing_offer(home: &Path) -> Result<Value> {
    let config = crate::config::resolve_persisted_for_pairing(home).await?;
    generate_local_pairing_offer(home, &config, true).await
}

pub(super) fn pairing_offer(identity: &Value, config: &Value, include_qr: bool) -> Result<Value> {
    if config["relayEnabled"].as_bool() == Some(false) {
        return Ok(json!({"relayEnabled":false,"url":null,"qr":null}));
    }
    // Original generateLocalPairingOffer: the private endpoint's TLS default
    // (hosted relay only) carries over to the public endpoint unless overridden.
    let relay_endpoint = config["relayEndpoint"]
        .as_str()
        .unwrap_or("relay.paseo.sh:443");
    let endpoint = config["relayPublicEndpoint"]
        .as_str()
        .unwrap_or(relay_endpoint);
    let relay_use_tls = config["relayUseTls"]
        .as_bool()
        .unwrap_or(relay_endpoint == "relay.paseo.sh:443");
    let use_tls = config["relayPublicUseTls"]
        .as_bool()
        .unwrap_or(relay_use_tls);
    let mut offer = json!({"v":2,"serverId":identity["serverId"],"daemonPublicKeyB64":identity["daemonPublicKey"],"relay":{"endpoint":endpoint,"useTls":use_tls}});
    if config["relayRequireProtectedSecurity"] == true {
        offer["minimumSecurityMode"] = json!("protected");
    }
    let base = config["appBaseUrl"]
        .as_str()
        .unwrap_or("https://app.paseo.sh")
        .trim_end_matches('/');
    let url = format!(
        "{base}/#offer={}",
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&offer)?)
    );
    // A QR rendering failure leaves the link usable (original logs and returns qr: null).
    let qr = if include_qr {
        super::render_qr(&url)
            .inspect_err(|error| tracing::debug!(%error, "Failed to render pairing QR"))
            .ok()
    } else {
        None
    };
    Ok(json!({"relayEnabled":true,"url":url,"qr":qr}))
}

pub async fn hash_daemon_password(password: &str) -> Result<String> {
    let password = password.to_owned();
    tokio::task::spawn_blocking(move || bcrypt::hash(password, 12))
        .await?
        .map_err(Into::into)
}
