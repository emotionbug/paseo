use super::super::error;
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};
use tokio::io::AsyncWriteExt;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Credential {
    pub origin: String,
    pub credential: String,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Store {
    version: u32,
    active_origin: Option<String>,
    credentials: Vec<Credential>,
}
impl Default for Store {
    fn default() -> Self {
        Self {
            version: 1,
            active_origin: None,
            credentials: Vec::new(),
        }
    }
}
pub(super) fn normalize(value: &str) -> Result<String> {
    let url = url::Url::parse(value).map_err(|_| invalid_origin())?;
    let loopback = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    if !(url.scheme() == "https" || url.scheme() == "http" && loopback)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(invalid_origin());
    }
    Ok(url.origin().ascii_serialization())
}
fn invalid_origin() -> anyhow::Error {
    error(
        "HUB_INVALID_ORIGIN",
        "Hub URL must be an HTTPS origin without credentials, path, query, or hash. HTTP is allowed only for localhost, 127.0.0.1, or [::1].",
    )
}
fn invalid_store() -> anyhow::Error {
    error(
        "HUB_CREDENTIALS_INVALID",
        "Stored Hub login is invalid. Run paseo hub login <origin> to replace it.",
    )
}
fn unavailable() -> anyhow::Error {
    error(
        "HUB_CREDENTIALS_UNAVAILABLE",
        "Could not access the private Hub credential store under PASEO_HOME.",
    )
}
async fn mode(path: &Path, mode: u32) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
            .await
            .map_err(|_| unavailable())?;
    }
    #[cfg(not(unix))]
    let _ = (path, mode);
    Ok(())
}
impl Store {
    pub async fn read(home: &Path) -> Result<Self> {
        let path = home.join("hub-credentials.json");
        let bytes = match tokio::fs::read(&path).await {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(_) => return Err(unavailable()),
        };
        mode(&path, 0o600).await?;
        let store: Self = serde_json::from_slice(&bytes).map_err(|_| invalid_store())?;
        store.validate()?;
        Ok(store)
    }
    fn validate(&self) -> Result<()> {
        let mut origins = HashSet::new();
        ensure!(self.version == 1, invalid_store());
        for credential in &self.credentials {
            ensure!(
                !credential.credential.is_empty()
                    && normalize(&credential.origin).ok().as_deref()
                        == Some(credential.origin.as_str())
                    && origins.insert(credential.origin.as_str()),
                invalid_store()
            );
        }
        ensure!(
            self.active_origin
                .as_ref()
                .is_none_or(|origin| origins.contains(origin.as_str())),
            invalid_store()
        );
        Ok(())
    }
    pub fn active(&self) -> Option<&Credential> {
        self.active_origin
            .as_ref()
            .and_then(|origin| self.credentials.iter().find(|item| &item.origin == origin))
    }
    pub fn contains(&self, origin: &str) -> bool {
        self.credentials.iter().any(|item| item.origin == origin)
    }
    pub fn resolve_origin(&self, input: Option<&str>) -> Result<String> {
        normalize(
            input
                .or(std::env::var("PASEO_HUB_URL").ok().as_deref())
                .or_else(|| self.active().map(|item| item.origin.as_str()))
                .unwrap_or("https://hub.paseo.sh"),
        )
    }
    pub fn resolve_key(&self, origin: &str, input: Option<&str>) -> Result<String> {
        if let Some(input) = input {
            return Ok(input.to_owned());
        }
        if let Ok(key) = std::env::var("PASEO_HUB_API_KEY") {
            return Ok(key);
        }
        self.credentials.iter().find(|item|item.origin==origin).map(|item|item.credential.clone()).ok_or_else(||error("HUB_API_KEY_REQUIRED",format!("No stored Hub login matches {origin}. Run paseo hub login {origin} or set PASEO_HUB_API_KEY.")))
    }
    pub async fn save(&mut self, home: &Path, credential: Credential) -> Result<()> {
        let origin = normalize(&credential.origin)?;
        self.credentials.retain(|item| item.origin != origin);
        self.credentials.push(Credential {
            origin: origin.clone(),
            credential: credential.credential,
        });
        self.active_origin = Some(origin);
        self.write(home).await
    }
    pub async fn logout(&mut self, home: &Path) -> Result<()> {
        if let Some(origin) = self.active_origin.take() {
            self.credentials.retain(|item| item.origin != origin);
            self.write(home).await?;
        }
        Ok(())
    }
    async fn write(&self, home: &Path) -> Result<()> {
        self.validate()?;
        tokio::fs::create_dir_all(home)
            .await
            .map_err(|_| unavailable())?;
        mode(home, 0o700).await?;
        let target = home.join("hub-credentials.json");
        let temporary = home.join(format!(
            ".hub-credentials.json.{}.{}.tmp",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let result = async {
            let mut options = tokio::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            options.mode(0o600);
            let mut file = options.open(&temporary).await?;
            let mut bytes = serde_json::to_vec_pretty(self)?;
            bytes.push(b'\n');
            file.write_all(&bytes).await?;
            file.sync_all().await?;
            drop(file);
            tokio::fs::rename(&temporary, &target).await?;
            Ok::<_, anyhow::Error>(())
        }
        .await;
        if result.is_err() {
            let _ = tokio::fs::remove_file(&temporary).await;
            return Err(unavailable());
        }
        mode(&target, 0o600).await
    }
}
pub(super) fn home() -> Result<PathBuf> {
    crate::client::resolve_home(std::env::var("PASEO_HOME").ok().as_deref())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn preserves_other_origins_and_private_modes() {
        let home = tempfile::tempdir().unwrap();
        let mut store = Store::read(home.path()).await.unwrap();
        store
            .save(
                home.path(),
                Credential {
                    origin: "https://one.example".into(),
                    credential: "first".into(),
                },
            )
            .await
            .unwrap();
        store
            .save(
                home.path(),
                Credential {
                    origin: "https://two.example".into(),
                    credential: "second".into(),
                },
            )
            .await
            .unwrap();
        store.logout(home.path()).await.unwrap();
        let store = Store::read(home.path()).await.unwrap();
        assert!(store.active().is_none());
        assert_eq!(
            store
                .credentials
                .iter()
                .find(|c| c.origin == "https://one.example")
                .unwrap()
                .credential,
            "first"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                tokio::fs::metadata(home.path().join("hub-credentials.json"))
                    .await
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }
    #[test]
    fn rejects_credential_disclosure_origins() {
        for value in [
            "http://public.example",
            "https://user:pass@example.com",
            "https://example.com/path",
            "https://example.com/?q=x",
        ] {
            assert!(normalize(value).is_err());
        }
        assert_eq!(
            normalize("http://[::1]:8080/").unwrap(),
            "http://[::1]:8080"
        );
    }
}
