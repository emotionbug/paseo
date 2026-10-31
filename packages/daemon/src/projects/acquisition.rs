use super::*;
use base64::{Engine, engine::general_purpose::STANDARD};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;

impl ProjectService {
    pub(super) async fn create_project_directory(&self, input: &Value) -> Result<Value> {
        let name = text(input, "name")?;
        if name.trim().is_empty()
            || name != name.trim()
            || matches!(name, "." | "..")
            || name.contains(['/', '\\', '\0'])
            || name.as_bytes().get(1) == Some(&b':')
        {
            return Err(RequestError::new("invalid_name","Directory name must be a single name without path separators or surrounding whitespace").into());
        }
        let parent = text(input, "parentPath")?.trim();
        if parent.is_empty() {
            return Err(RequestError::new(
                "parent_directory_not_found",
                "Parent directory is required",
            )
            .into());
        }
        let parent = self.absolute(Path::new(parent))?;
        let meta = tokio::fs::metadata(&parent)
            .await
            .map_err(|e| directory_error(e, &parent, false))?;
        if !meta.is_dir() {
            return Err(RequestError::new(
                "parent_directory_not_found",
                format!("Parent directory not found: {}", parent.display()),
            )
            .into());
        }
        let path = parent.join(name);
        tokio::fs::create_dir(&path)
            .await
            .map_err(|e| directory_error(e, &path, true))?;
        match self.register_project(&path).await {
            Ok(p) => Ok(
                json!({"directoryPath":path,"project":directory::project_descriptor(&p),"error":null,"errorCode":null}),
            ),
            Err(error) => {
                let message = match tokio::fs::remove_dir(&path).await {
                    Ok(()) => format!("Failed to register project: {error}"),
                    Err(cleanup) => {
                        format!("Failed to register project and roll back directory: {cleanup}")
                    }
                };
                Err(RequestError::new("registration_failed", message).into())
            }
        }
    }
    fn icon_path(&self, id: &str) -> Result<PathBuf> {
        Ok(self
            .app()?
            .home
            .join("projects")
            .join("icons")
            .join(format!("{:x}.bin", Sha256::digest(id.as_bytes()))))
    }
    pub(super) async fn remove_icon(&self, id: &str) -> Result<()> {
        match tokio::fs::remove_file(self.icon_path(id)?).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
    pub(super) async fn icon_snapshot(&self, project: &Value) -> Result<Value> {
        let snapshot = self.observe_icon(project).await?;
        self.icon_snapshots
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(text(project, "projectId")?.into(), snapshot.clone());
        Ok(snapshot)
    }
    pub(super) async fn cached_icon(&self, project: &Value) -> Result<Value> {
        let cached = {
            self.icon_snapshots
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .get(text(project, "projectId")?)
                .cloned()
        };
        match cached {
            Some(snapshot) => Ok(snapshot["icon"].clone()),
            None => Ok(self.icon_snapshot(project).await?["icon"].clone()),
        }
    }
    async fn observe_icon(&self, project: &Value) -> Result<Value> {
        let (icon, source) = if let Some(revision) = project["customIconRevision"].as_str() {
            let icon = match tokio::fs::read(self.icon_path(text(project, "projectId")?)?).await {
                Ok(bytes) => validate_icon(&bytes).ok(),
                Err(_) => None,
            };
            if icon.is_none() {
                return Ok(json!({"icon":null,"revision":format!("custom:none:{revision}")}));
            }
            (icon, "custom")
        } else {
            (
                crate::files::project_icon(Path::new(text(project, "rootPath")?)).await?,
                "automatic",
            )
        };
        let Some(icon) = icon else {
            return Ok(json!({"icon":null,"revision":"automatic:none:v1"}));
        };
        let mut digest = Sha256::new();
        digest.update(text(&icon, "mimeType")?);
        digest.update([0]);
        digest.update(text(&icon, "data")?);
        Ok(json!({"icon":icon,"revision":format!("{source}:{:x}",digest.finalize())}))
    }
    pub(super) async fn set_icon(&self, m: &Value) -> Result<Value> {
        let _guard = self.lifecycle.lock().await;
        let id = text(m, "projectId")?;
        self.project(id)
            .await?
            .ok_or_else(|| anyhow!("Project not found"))?;
        let source = &m["source"];
        let revision = if source["type"] == "automatic" {
            self.remove_icon(id).await?;
            Value::Null
        } else {
            let bytes = STANDARD
                .decode(text(source, "data")?)
                .context("Unsupported or invalid icon file")?;
            validate_icon(&bytes)?;
            atomic_bytes(&self.icon_path(id)?, &bytes).await?;
            json!(uuid::Uuid::new_v4().to_string())
        };
        let project = self
            .storage()
            .await?
            .update_project(id, |p| {
                let mut next = p.clone();
                next["customIconRevision"] = revision.clone();
                next["updatedAt"] = json!(now());
                Ok(next)
            })
            .await?
            .ok_or_else(|| anyhow!("Project not found"))?;
        self.publish_project(&project).await?;
        for workspace in self.list_workspaces().await? {
            if workspace["projectId"] == id {
                self.publish_workspace(text(&workspace, "workspaceId")?)
                    .await?;
            }
        }
        Ok(json!({"projectId":id,"accepted":true,"error":null}))
    }
    pub(super) async fn search_repositories(
        &self,
        m: &Value,
        cancel: CancellationToken,
    ) -> Result<Value> {
        let app = self.app()?;
        match app.git.forge.execute(&app.user_home,"searchRepositories",&json!({"forge":"github","query":m["query"],"limit":m.get("limit").cloned().unwrap_or(json!(20))}),&app.config.environment(),cancel).await{
            Ok(repositories)=>Ok(json!({"repositories":repositories,"status":"success","available":true,"error":null})),
            Err(error)=>{
                let state=crate::git::forge::auth_state(&error);
                let (status,available,message)=match state{"cli_missing"=>("unavailable",false,"GitHub CLI (gh) is not installed or not in PATH".to_owned()),"unauthenticated"=>("unauthenticated",false,"GitHub CLI is not authenticated. Run gh auth login on the host.".to_owned()),_=>("error",true,error.downcast_ref::<crate::git::forge::ForgeError>().map(|e|e.stderr.trim()).filter(|e|!e.is_empty()).unwrap_or(&error.to_string()).to_owned())};
                let mut payload=json!({"repositories":[],"status":status,"available":available,"error":message});if state=="cli_missing"{payload["reason"]=json!("gh_missing");}Ok(payload)
            }
        }
    }
    pub(super) async fn clone_project(
        &self,
        m: &Value,
        cancel: CancellationToken,
    ) -> Result<Value> {
        let repo = crate::git::forge::normalize_clone_repository(
            text(m, "repo")?,
            m["cloneProtocol"].as_str(),
        )?;
        let parent = self.absolute(Path::new(text(m, "targetDirectory")?.trim()))?;
        let checkout = parent.join(text(&repo, "name")?);
        if checkout.parent() != Some(parent.as_path()) {
            bail!("Resolved checkout path must stay inside the target directory")
        }
        tokio::fs::create_dir_all(&parent).await?;
        match tokio::fs::symlink_metadata(&checkout).await {
            Ok(_) => bail!("Checkout path already exists: {}", checkout.display()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        let staging = parent.join(format!(".paseo-clone-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir(&staging).await?;
        let app = self.app()?;
        let result = async {
            crate::git::command::checked(
                &parent,
                &[
                    "clone".into(),
                    text(&repo, "cloneUrl")?.into(),
                    staging.to_string_lossy().into_owned(),
                ],
                &app.config.environment(),
                cancel,
                1024 * 1024,
                std::time::Duration::from_secs(300),
            )
            .await?;
            rename_directory_noreplace(&staging, &checkout).await?;
            Ok::<_, anyhow::Error>(())
        }
        .await;
        if let Err(error) = result {
            if let Err(cleanup) = tokio::fs::remove_dir_all(&staging).await {
                tracing::warn!(%cleanup,"Failed to clean owned clone staging directory");
            }
            return Ok(
                json!({"repo":repo["displayName"],"checkoutPath":checkout,"project":null,"error":error.to_string()}),
            );
        }
        let project = self.register_project(&checkout).await?;
        Ok(
            json!({"repo":repo["displayName"],"checkoutPath":checkout,"project":directory::project_descriptor(&project),"error":null}),
        )
    }
}
fn directory_error(error: std::io::Error, path: &Path, creating: bool) -> RequestError {
    let code = match error.kind() {
        std::io::ErrorKind::AlreadyExists => "directory_exists",
        std::io::ErrorKind::PermissionDenied => "permission_denied",
        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory => {
            "parent_directory_not_found"
        }
        _ => "filesystem_error",
    };
    RequestError::new(
        code,
        format!(
            "Failed to {} directory {}: {error}",
            if creating { "create" } else { "inspect" },
            path.display()
        ),
    )
}
fn validate_icon(bytes: &[u8]) -> Result<Value> {
    if bytes.is_empty() || bytes.len() > 512 * 1024 {
        bail!("Icon must be 512 KB or smaller")
    }
    let mime = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        "image/png"
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        "image/jpeg"
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        "image/gif"
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        "image/webp"
    } else if bytes.len() >= 22
        && bytes[..4] == [0, 0, 1, 0]
        && u16::from_le_bytes([bytes[4], bytes[5]]) > 0
    {
        "image/x-icon"
    } else {
        bail!("Unsupported or invalid icon file")
    };
    let Some((w, h)) = crate::files::image_dimensions(bytes, mime) else {
        bail!("Icon must be a square image up to 1024×1024")
    };
    if w == 0 || w != h || w > 1024 {
        bail!("Icon must be a square image up to 1024×1024")
    }
    Ok(json!({"mimeType":mime,"data":STANDARD.encode(bytes)}))
}
async fn atomic_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("No icon parent directory")?;
    tokio::fs::create_dir_all(parent).await?;
    let temp = parent.join(format!(".paseo-icon-{}", uuid::Uuid::new_v4()));
    let result = async {
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .await?;
        file.write_all(bytes).await?;
        file.sync_all().await?;
        drop(file);
        tokio::fs::rename(&temp, path).await?;
        Ok::<_, anyhow::Error>(())
    }
    .await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(&temp).await;
    }
    result
}
async fn rename_directory_noreplace(from: &Path, to: &Path) -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::ffi::OsStrExt;
        let from = std::ffi::CString::new(from.as_os_str().as_bytes())?;
        let to = std::ffi::CString::new(to.as_os_str().as_bytes())?;
        tokio::task::spawn_blocking(move || {
            let result = unsafe {
                libc::renameat2(
                    libc::AT_FDCWD,
                    from.as_ptr(),
                    libc::AT_FDCWD,
                    to.as_ptr(),
                    libc::RENAME_NOREPLACE,
                )
            };
            if result == 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        })
        .await??;
        Ok(())
    }
    #[cfg(not(target_os = "linux"))]
    {
        if tokio::fs::symlink_metadata(to).await.is_ok() {
            bail!("Checkout path already exists: {}", to.display())
        }
        tokio::fs::rename(from, to).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn cloned_directory_never_replaces_existing_destination() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("owned-staging");
        let target = temp.path().join("user-checkout");
        tokio::fs::create_dir(&source).await.unwrap();
        tokio::fs::create_dir(&target).await.unwrap();
        tokio::fs::write(target.join("user-file"), b"preserve")
            .await
            .unwrap();
        assert!(rename_directory_noreplace(&source, &target).await.is_err());
        assert_eq!(
            tokio::fs::read(target.join("user-file")).await.unwrap(),
            b"preserve"
        );
        assert!(source.exists());
    }
    #[tokio::test]
    async fn atomic_icon_bytes_replace_durably() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("icons/key.bin");
        atomic_bytes(&path, b"first").await.unwrap();
        atomic_bytes(&path, b"second").await.unwrap();
        assert_eq!(tokio::fs::read(path).await.unwrap(), b"second");
    }
}
