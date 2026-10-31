use super::manifest;
use crate::process::{CommandSpec, ManagedProcess};
use anyhow::{Context, Result, bail};
use serde_json::{Map, Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::RwLock,
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::Mutex,
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

mod git;
mod npm;
mod reference;
mod registry;
use reference::{absolute, plugin_path, string};

pub struct Candidate {
    pub directory: PathBuf,
    pub default_id: String,
    pub record: Value,
    pub build: Value,
    pub version_root: Option<PathBuf>,
    pub target: Option<Value>,
}

struct Location {
    version_root: PathBuf,
    source_root: PathBuf,
    identity: Value,
}

pub struct ManagedSources {
    root: PathBuf,
    env: BTreeMap<String, String>,
    records: Mutex<Option<Map<String, Value>>>,
    registry_options: RwLock<Value>,
}

impl ManagedSources {
    pub fn new(home: PathBuf, env: BTreeMap<String, String>) -> Self {
        Self {
            root: home.join("plugins"),
            env,
            records: Mutex::new(None),
            registry_options: RwLock::new(json!({})),
        }
    }

    pub fn configure_registry(&self, config: &Value) -> Result<()> {
        let mut options = json!({});
        if let Some(registries) = config.get("pluginRegistries") {
            for value in registries
                .as_object()
                .context("Invalid pluginRegistries")?
                .values()
            {
                string(value, "authorization")?;
            }
            options["pluginRegistries"] = registries.clone();
        }
        let default = config
            .get("pluginRegistryUrl")
            .and_then(Value::as_str)
            .or_else(|| self.env.get("PASEO_PLUGIN_REGISTRY").map(String::as_str))
            .unwrap_or("https://plugins.paseo.sh");
        url::Url::parse(default).context("Invalid plugin registry URL")?;
        options["pluginRegistryUrl"] = json!(default.trim_end_matches('/'));
        *self
            .registry_options
            .write()
            .map_err(|_| anyhow::anyhow!("Plugin registry configuration lock poisoned"))? = options;
        Ok(())
    }

    fn options(&self) -> Result<Value> {
        Ok(self
            .registry_options
            .read()
            .map_err(|_| anyhow::anyhow!("Plugin registry configuration lock poisoned"))?
            .clone())
    }

    pub async fn initialize(&self) -> Result<()> {
        let mut records = self.records.lock().await;
        if records.is_some() {
            return Ok(());
        }
        private_directory(&self.root).await?;
        let path = self.root.join("sources.json");
        let loaded = match tokio::fs::symlink_metadata(&path).await {
            Ok(metadata) => {
                if !metadata.is_file() || metadata.file_type().is_symlink() {
                    bail!("Plugin sources registry must be a regular file");
                }
                let value = read_json(&path).await?;
                let mut loaded = Map::new();
                for (id, record) in value
                    .as_object()
                    .context("Plugin sources registry must be an object")?
                {
                    manifest::validate_id(id)?;
                    loaded.insert(id.clone(), normalize_record(record)?);
                }
                loaded
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Map::new(),
            Err(error) => return Err(error.into()),
        };
        *records = Some(loaded);
        Ok(())
    }

    async fn record(&self, id: &str) -> Result<Option<Value>> {
        manifest::validate_id(id)?;
        let records = self.records.lock().await;
        Ok(records
            .as_ref()
            .context("Managed plugin sources are not initialized")?
            .get(id)
            .cloned())
    }

    fn locate(&self, id: &str, configured: &Path, record: &Value) -> Result<Location> {
        manifest::validate_id(id)?;
        let plugin_root = absolute(&self.root.join(id))?;
        let configured = absolute(configured)?;
        let relative = configured.strip_prefix(&plugin_root).with_context(|| {
            format!("Plugin directory is outside its managed installation: {id}")
        })?;
        let parts: Vec<_> = relative
            .iter()
            .map(|s| s.to_str().context("Plugin path is not UTF-8"))
            .collect::<Result<_>>()?;
        let version_name = parts
            .first()
            .context("Missing plugin installation version")?;
        let version_id = if record["kind"] == "git"
            && version_name.len() == 49
            && version_name.as_bytes()[12] == b'-'
            && version_name[..12]
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            &version_name[13..]
        } else {
            version_name
        };
        if Uuid::parse_str(version_id)
            .ok()
            .is_none_or(|id| id.to_string() != version_id)
        {
            bail!("Plugin directory is outside its managed installation: {id}");
        }
        let version_root = plugin_root.join(version_name);
        let (source_root, mut identity) = if record["kind"] == "git" {
            if parts.get(1) != Some(&"checkout") {
                bail!("Invalid Git installation directory: {id}");
            }
            (
                version_root.join("checkout"),
                json!({"kind":"git","remote":reference::redact(string(record,"remote")?),"pluginPath":join_path(&parts[2..])}),
            )
        } else {
            if parts.get(1) != Some(&"node_modules") || parts.get(2).is_none() {
                bail!("Invalid npm installation directory: {id}");
            }
            let package_parts = if parts[2].starts_with('@') { 2 } else { 1 };
            if parts.len() < 2 + package_parts {
                bail!("Invalid npm package directory: {id}");
            }
            let package = parts[2..2 + package_parts].join("/");
            let (name, _) = npm::parse(&package)?;
            if name != package {
                bail!("Invalid npm package directory: {id}");
            }
            (
                version_root.join("node_modules").join(&package),
                json!({"kind":"npm","packageName":package,"pluginPath":join_path(&parts[2 + package_parts..])}),
            )
        };
        if let Some(registry) = record.get("registry") {
            identity["registry"] = registry.clone();
        }
        Ok(Location {
            version_root,
            source_root,
            identity,
        })
    }

    async fn revision(&self, location: &Location, configured: &Path) -> Result<String> {
        containment(&self.root, &location.version_root).await?;
        containment(&location.version_root, configured).await?;
        if location.identity["kind"] == "npm" {
            Ok(string(
                &npm::artifact(
                    &location.version_root,
                    string(&location.identity, "packageName")?,
                )
                .await?,
                "version",
            )?
            .to_owned())
        } else {
            git::rev_parse(&location.source_root, "HEAD", &self.env).await
        }
    }

    pub async fn describe(&self, id: &str, configured: &Path) -> Result<Value> {
        let Some(record) = self.record(id).await? else {
            return Ok(json!({"identity":{"kind":"directory","path":absolute(configured)?}}));
        };
        let location = self.locate(id, configured, &record)?;
        let revision = self.revision(&location, configured).await;
        let mut result = json!({"identity":location.identity});
        if let Ok(revision) = revision {
            result["currentRevision"] = json!(revision);
        }
        Ok(result)
    }

    pub async fn prepare_install(&self, input: &Value) -> Result<Candidate> {
        let source = string(input, "source")?;
        let options = self.options()?;
        if let Some(identity) = reference::registry(
            source,
            options["pluginRegistryUrl"]
                .as_str()
                .unwrap_or("https://plugins.paseo.sh"),
        )? {
            if input.get("ref").is_some() {
                bail!(
                    "Registry installs pin the reviewed revision. Drop --ref, or install from an explicit github:owner/repository source to choose a revision."
                );
            }
            if input.get("pluginPath").is_some() {
                bail!("Registry sources use their reviewed artifact path and revision");
            }
            let resolved = registry::resolve(&identity, &options, true).await?;
            let mut candidate = self.acquire(&resolved.input, Some(resolved.target)).await?;
            candidate.record["registry"] = identity;
            return Ok(candidate);
        }
        let (source, nested) = reference::split(source);
        let mut input = input.clone();
        input["source"] = json!(source);
        if let Some(nested) = nested {
            if input
                .get("pluginPath")
                .is_some_and(|value| value.as_str() != Some(nested.as_str()))
            {
                bail!("Conflicting plugin paths");
            }
            input["pluginPath"] = json!(nested);
        }
        let source = string(&input, "source")?;
        if reference::registry(
            source,
            options["pluginRegistryUrl"]
                .as_str()
                .unwrap_or("https://plugins.paseo.sh"),
        )?
        .is_some()
        {
            if input.get("ref").is_some() {
                bail!(
                    "Registry installs pin the reviewed revision. Drop --ref, or install from an explicit github:owner/repository source to choose a revision."
                );
            }
            bail!("Registry sources use their reviewed artifact path and revision");
        }
        let home = self
            .env
            .get("HOME")
            .or_else(|| self.env.get("USERPROFILE"))
            .map(PathBuf::from)
            .or_else(dirs::home_dir)
            .context("User home directory is unavailable")?;
        let local = manifest::resolve_directory(source, &home)?;
        let explicit_local =
            source.starts_with(['.', '/', '~', '\\']) || source.as_bytes().get(1) == Some(&b':');
        let exists = match tokio::fs::metadata(&local).await {
            Ok(metadata) => metadata.is_dir(),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) =>
            {
                false
            }
            Err(error) => return Err(error.into()),
        };
        if exists || explicit_local {
            if input.get("ref").is_some() {
                bail!("Plugin --ref is only valid for Git sources");
            }
            let directory = absolute(
                &local.join(plugin_path(
                    input
                        .get("pluginPath")
                        .map(|v| v.as_str().context("Invalid pluginPath"))
                        .transpose()?,
                )?),
            )?;
            containment(&local, &directory).await?;
            let manifest = manifest::read(&directory).await?;
            return Ok(Candidate {
                directory,
                default_id: string(&manifest, "id")?.into(),
                record: Value::Null,
                build: manifest.get("build").cloned().unwrap_or(Value::Null),
                version_root: None,
                target: None,
            });
        }
        self.acquire(&input, None).await
    }

    async fn acquire(&self, input: &Value, target: Option<Value>) -> Result<Candidate> {
        let source = string(input, "source")?;
        let path = plugin_path(
            input
                .get("pluginPath")
                .map(|v| v.as_str().context("Invalid pluginPath"))
                .transpose()?,
        )?;
        let requested = input
            .get("ref")
            .map(|v| v.as_str().context("Invalid Git ref"))
            .transpose()?;
        let root = self.staging().await?;
        let mut cleanup = StagingCleanup(Some(root.clone()));
        let result = async {
            let (source_root, record) = if npm::is_source(source) {
                if requested.is_some() { bail!("Plugin --ref is only valid for Git sources; use npm:package@version for npm"); }
                let package = npm::acquire(source, &root, target.as_ref(), &self.env).await?;
                (root.join("node_modules").join(package), json!({"kind":"npm"}))
            } else {
                let remote = git::normalize(source)?;
                let checkout = root.join("checkout");
                git::clone(&remote, &checkout, &self.env).await?;
                let commit = git::requested(&checkout, requested, &self.env).await?;
                if target.as_ref().is_some_and(|target| target["kind"] != "git" || target["commit"] != commit) { bail!("Git target changed since review"); }
                git::checkout(&checkout, &commit, &self.env).await?;
                (checkout, json!({"kind":"git","remote":remote}))
            };
            let directory = source_root.join(&path);
            containment(&source_root, &directory).await?;
            let manifest = manifest::read(&directory).await?;
            Ok(Candidate { directory, default_id:string(&manifest,"id")?.into(), record, build:manifest.get("build").cloned().unwrap_or(Value::Null), version_root:Some(root.clone()), target })
        }.await;
        if result.is_err() {
            remove_tree(&root)
                .await
                .context("Failed to clean rejected plugin candidate")?;
        }
        cleanup.0 = None;
        result
    }

    pub async fn place(&self, id: &str, mut candidate: Candidate) -> Result<Candidate> {
        let mut cleanup = StagingCleanup(None);
        let result = async {
            manifest::validate_id(id)?;
            let Some(old_root) = candidate.version_root.as_ref() else {
                return Ok::<_, anyhow::Error>(());
            };
            self.owned_root(old_root).await?;
            cleanup.0 = Some(old_root.clone());
            let relative = candidate
                .directory
                .strip_prefix(old_root)
                .context("Candidate directory is outside its installation")?
                .to_owned();
            containment(old_root, &candidate.directory).await?;
            let plugin_root = self.root.join(id);
            private_directory(&plugin_root).await?;
            containment(&self.root, &plugin_root).await?;
            let root = plugin_root.join(Uuid::new_v4().to_string());
            tokio::fs::rename(old_root, &root).await?;
            cleanup.0 = Some(root.clone());
            candidate.directory = root.join(relative);
            candidate.version_root = Some(root);
            Ok(())
        }
        .await;
        if let Err(error) = result {
            self.discard(&candidate)
                .await
                .context("Failed to clean candidate after placement failure")?;
            cleanup.0 = None;
            return Err(error);
        }
        cleanup.0 = None;
        Ok(candidate)
    }

    pub async fn preview(
        &self,
        id: &str,
        configured: &Path,
        selection: Option<&Value>,
    ) -> Result<Value> {
        let current = self.describe(id, configured).await?;
        let identity = &current["identity"];
        if identity["kind"] == "directory" {
            if selection.is_some() {
                bail!("Plugin is a local directory: {id}");
            }
            return Ok(json!({"id":id,"outcome":"local","current":current,"links":[]}));
        }
        let revision = current["currentRevision"].as_str().with_context(|| format!("Installed revision is unavailable for {id}; restore its acquisition artifacts before updating"))?;
        if selection.is_some_and(|s| s["kind"] != identity["kind"]) {
            bail!(
                "Update target does not match {} source: {id}",
                identity["kind"]
            );
        }
        let record = self
            .record(id)
            .await?
            .context("Plugin source is no longer configured")?;
        let location = self.locate(id, configured, &record)?;
        let target = if let Some(registry_identity) = identity.get("registry") {
            if selection.is_some() {
                bail!("Registry updates use only the reviewed registry pin");
            }
            let resolved = registry::resolve(registry_identity, &self.options()?, false).await?;
            registry::assert_identity(identity, &resolved.artifact)?;
            resolved.target
        } else if identity["kind"] == "npm" {
            let selector = selection
                .map(|s| string(s, "version"))
                .transpose()?
                .unwrap_or("latest");
            npm::resolve(
                string(identity, "packageName")?,
                selector,
                &self.root,
                &self.env,
            )
            .await?
        } else {
            let requested = selection.map(|s| string(s, "ref")).transpose()?;
            json!({"kind":"git","commit":git::remote_commit(self,string(&record,"remote")?,requested).await?})
        };
        let target_revision = string(
            &target,
            if target["kind"] == "npm" {
                "version"
            } else {
                "commit"
            },
        )?;
        let outcome = if identity["kind"] == "npm" && identity.get("registry").is_none() {
            let ordering = semver::Version::parse(target_revision)?
                .cmp_precedence(&semver::Version::parse(revision)?);
            if ordering.is_eq() {
                "current"
            } else if ordering.is_lt() && selection.is_none() {
                "installed-newer"
            } else {
                "update"
            }
        } else if target_revision == revision {
            "current"
        } else {
            "update"
        };
        let mut result = json!({"id":id,"outcome":outcome,"current":current,"target":target,"links":reference::links(&current,&target)});
        if outcome == "update" {
            result["proposal"] = json!({"id":id,"expected":{"identity":identity,"installationRoot":location.version_root,"revision":revision},"target":target});
        }
        Ok(result)
    }

    pub async fn assert_current(&self, proposal: &Value, configured: &Path) -> Result<()> {
        let id = string(proposal, "id")?;
        let current = self.describe(id, configured).await?;
        let record = self
            .record(id)
            .await?
            .with_context(|| format!("Plugin is a local directory: {id}"))?;
        let location = self.locate(id, configured, &record)?;
        if current["identity"] != proposal["expected"]["identity"]
            || current.get("currentRevision").is_none()
            || current["currentRevision"] != proposal["expected"]["revision"]
            || json!(location.version_root) != proposal["expected"]["installationRoot"]
        {
            bail!("Plugin {id} changed since review; check again");
        }
        if proposal["target"]["kind"] != current["identity"]["kind"] {
            bail!("Update target does not match the installed source");
        }
        validate_target(&proposal["target"])?;
        Ok(())
    }

    pub async fn prepare_update(&self, proposal: &Value, configured: &Path) -> Result<Candidate> {
        self.assert_current(proposal, configured).await?;
        let id = string(proposal, "id")?;
        let record = self
            .record(id)
            .await?
            .context("Plugin source is no longer configured")?;
        let identity = self.locate(id, configured, &record)?.identity;
        let target = &proposal["target"];
        if let Some(registry_identity) = identity.get("registry") {
            let resolved = registry::resolve(registry_identity, &self.options()?, false).await?;
            registry::assert_identity(&identity, &resolved.artifact)?;
            if resolved.target != *target {
                bail!("Registry pin changed since review; check again");
            }
            let mut candidate = self.acquire(&resolved.input, Some(target.clone())).await?;
            candidate.record["registry"] = registry_identity.clone();
            return Ok(candidate);
        }
        let input = if identity["kind"] == "git" {
            json!({"source":format!("git:{}",string(&record,"remote")?),"ref":target["commit"],"pluginPath":identity["pluginPath"]})
        } else {
            json!({"source":format!("npm:{}@{}",string(&identity,"packageName")?,string(target,"version")?),"pluginPath":identity["pluginPath"]})
        };
        self.acquire(&input, Some(target.clone())).await
    }

    pub async fn verify(
        &self,
        id: &str,
        candidate: &Candidate,
        target: Option<&Value>,
    ) -> Result<()> {
        if candidate.record.is_null() {
            manifest::read(&candidate.directory).await?;
            return Ok(());
        }
        let location = self.locate(id, &candidate.directory, &candidate.record)?;
        let revision = self.revision(&location, &candidate.directory).await?;
        if let Some(target) = target.or(candidate.target.as_ref()) {
            validate_target(target)?;
            if target["kind"] != location.identity["kind"] {
                bail!("Update target does not match the installed source");
            }
            if target["kind"] == "git" && target["commit"] != revision {
                bail!("Preparation changed the reviewed Git commit");
            }
            if target["kind"] == "npm" {
                let artifact = npm::artifact(
                    &location.version_root,
                    string(&location.identity, "packageName")?,
                )
                .await?;
                if ["version", "integrity", "resolved"]
                    .iter()
                    .any(|key| artifact[*key] != target[*key])
                {
                    bail!("Preparation changed the reviewed npm artifact");
                }
            }
        }
        Ok(())
    }

    pub async fn build(&self, candidate: &Candidate) -> Result<()> {
        if candidate.build.is_null() {
            return Ok(());
        }
        for command in candidate
            .build
            .as_array()
            .context("Invalid plugin build commands")?
        {
            let args = command
                .as_array()
                .context("Invalid plugin build command")?
                .iter()
                .map(|v| {
                    v.as_str()
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned)
                        .context("Invalid plugin build argument")
                })
                .collect::<Result<Vec<_>>>()?;
            let program = args.first().context("Empty plugin build command")?.clone();
            let formatted = serde_json::to_string(command)?;
            tracing::info!(directory=%candidate.directory.display(), command=%formatted, "Running plugin build command");
            let (process, mut stdout) = ManagedProcess::spawn(CommandSpec {
                program,
                args: args.into_iter().skip(1).collect(),
                cwd: Some(candidate.directory.clone()),
                env: self.env.clone(),
                clear_env: true,
            })
            .await
            .with_context(|| format!("Plugin build command failed to start: {formatted}"))?;
            process.close_stdin().await?;
            let output = async {
                let mut tail = Vec::new();
                let mut buffer = [0u8; 8192];
                loop {
                    let count = stdout.read(&mut buffer).await?;
                    if count == 0 {
                        break;
                    }
                    let overflow = (tail.len() + count).saturating_sub(64 * 1024);
                    if overflow > 0 {
                        tail.drain(..overflow);
                    }
                    tail.extend_from_slice(&buffer[..count]);
                }
                Ok::<_, anyhow::Error>(String::from_utf8_lossy(&tail).into_owned())
            };
            let (status, stdout) = tokio::try_join!(process.wait(), output)?;
            let stderr = process.stderr_tail();
            if !stdout.is_empty() {
                tracing::info!(output=%stdout,"Plugin build stdout");
            }
            if !stderr.is_empty() {
                tracing::info!(output=%stderr,"Plugin build stderr");
            }
            if status != 0 {
                bail!(
                    "Plugin build command failed (exit {status}): {formatted}\n{stdout}\n{stderr}"
                );
            }
        }
        Ok(())
    }

    pub async fn status(&self, id: &str, configured: &Path) -> Result<Value> {
        let preview = self.preview(id, configured, None).await?;
        let installation = &preview["current"];
        let is_git = installation["identity"]["kind"] == "git";
        let mut status = json!({"id":id,"source":if is_git {"git"} else {"directory"},"path":configured,"installation":installation});
        if is_git {
            status["remote"] = installation["identity"]["remote"].clone();
            if let Some(commit) = installation.get("currentRevision") {
                status["currentCommit"] = commit.clone();
            }
            if let Some(commit) = preview["target"].get("commit") {
                status["latestCommit"] = commit.clone();
            }
            status["updateAvailable"] = json!(preview["outcome"] == "update");
        }
        Ok(status)
    }

    pub async fn commit(&self, id: &str, record: &Value) -> Result<()> {
        manifest::validate_id(id)?;
        let record = if record.is_null() {
            None
        } else {
            Some(normalize_record(record)?)
        };
        let mut records = self.records.lock().await;
        let mut next = records
            .as_ref()
            .context("Managed plugin sources are not initialized")?
            .clone();
        if let Some(record) = record {
            next.insert(id.to_owned(), record);
        } else {
            next.remove(id);
        }
        atomic_registry(&self.root, &next).await?;
        *records = Some(next);
        Ok(())
    }

    async fn owned_root(&self, root: &Path) -> Result<()> {
        let base = absolute(&self.root)?;
        let root = absolute(root)?;
        let parts: Vec<_> = root
            .strip_prefix(&base)
            .context("Candidate is outside managed plugins")?
            .components()
            .collect();
        if parts.len() != 2 {
            bail!("Invalid managed version directory");
        }
        let owner = parts[0]
            .as_os_str()
            .to_str()
            .context("Invalid managed owner")?;
        if owner != ".staging" {
            manifest::validate_id(owner)?;
        }
        let name = parts[1]
            .as_os_str()
            .to_str()
            .context("Invalid managed version")?;
        let legacy = name.len() == 49
            && name.as_bytes()[12] == b'-'
            && name[..12]
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
        let uuid = if legacy { &name[13..] } else { name };
        if Uuid::parse_str(uuid)
            .ok()
            .is_none_or(|id| id.to_string() != uuid)
        {
            bail!("Invalid managed version directory");
        }
        containment(&self.root, root.parent().context("Missing version parent")?).await?;
        Ok(())
    }

    pub async fn discard(&self, candidate: &Candidate) -> Result<()> {
        if let Some(root) = &candidate.version_root {
            self.owned_root(root).await?;
            remove_tree(root).await?;
        }
        Ok(())
    }

    pub async fn remove_version(&self, id: &str, configured: &Path) -> Result<()> {
        let record = self
            .record(id)
            .await?
            .with_context(|| format!("Plugin is a local directory: {id}"))?;
        let location = self.locate(id, configured, &record)?;
        self.owned_root(&location.version_root).await?;
        remove_tree(&location.version_root).await
    }

    pub async fn remove(&self, id: &str) -> Result<()> {
        manifest::validate_id(id)?;
        let mut records = self.records.lock().await;
        let current = records
            .as_ref()
            .context("Managed plugin sources are not initialized")?;
        if !current.contains_key(id) {
            return Ok(());
        }
        let root = self.root.join(id);
        let quarantine = self.root.join(".staging").join(Uuid::new_v4().to_string());
        let present = match tokio::fs::symlink_metadata(&root).await {
            Ok(_) => true,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            Err(e) => return Err(e.into()),
        };
        if present {
            containment(&self.root, &root).await?;
            private_directory(&self.root.join(".staging")).await?;
            containment(&self.root, &self.root.join(".staging")).await?;
            tokio::fs::rename(&root, &quarantine).await?;
        }
        let mut next = current.clone();
        next.remove(id);
        if let Err(error) = atomic_registry(&self.root, &next).await {
            if present {
                tokio::fs::rename(&quarantine, &root)
                    .await
                    .context("Failed to roll back managed plugin removal")?;
            }
            return Err(error);
        }
        *records = Some(next);
        if present {
            remove_tree(&quarantine).await?;
        }
        Ok(())
    }

    async fn staging(&self) -> Result<PathBuf> {
        let parent = self.root.join(".staging");
        private_directory(&parent).await?;
        containment(&self.root, &parent).await?;
        let root = parent.join(Uuid::new_v4().to_string());
        private_directory(&root).await?;
        containment(&self.root, &root).await?;
        Ok(root)
    }
}

fn join_path(parts: &[&str]) -> String {
    if parts.is_empty() {
        ".".to_owned()
    } else {
        parts.join("/")
    }
}

/// A dropped acquisition future still owns its staging tree. Cleanup is never
/// armed for a directory supplied by the user or an installed generation.
struct StagingCleanup(Option<PathBuf>);

impl Drop for StagingCleanup {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn(async move {
                    if let Err(error) = remove_tree(&path).await {
                        tracing::warn!(path=%path.display(), %error, "Failed to remove abandoned plugin staging directory");
                    }
                });
            }
        }
    }
}

fn normalize_record(record: &Value) -> Result<Value> {
    record
        .as_object()
        .context("Invalid managed plugin source record")?;
    let kind = record
        .get("kind")
        .map(|value| value.as_str().context("Invalid managed plugin source kind"))
        .transpose()?
        .unwrap_or("git");
    let mut normalized = match kind {
        "git" => json!({"kind":"git","remote":string(record,"remote")?}),
        "npm" => json!({"kind":"npm"}),
        _ => bail!("Invalid managed plugin source kind"),
    };
    if let Some(registry) = record.get("registry") {
        let id = string(registry, "id")?;
        if !reference::registry_id(id) {
            bail!("Invalid plugin registry ID");
        }
        let url = string(registry, "url")?;
        url::Url::parse(url)?;
        normalized["registry"] = json!({"id":id,"url":url});
    }
    Ok(normalized)
}

fn validate_target(target: &Value) -> Result<()> {
    match string(target, "kind")? {
        "git" => {
            if !reference::is_commit(string(target, "commit")?) {
                bail!("Invalid Git target commit");
            }
        }
        "npm" => {
            semver::Version::parse(string(target, "version")?)?;
            url::Url::parse(string(target, "resolved")?)?;
            string(target, "integrity")?;
        }
        _ => bail!("Invalid plugin update target"),
    }
    Ok(())
}

async fn containment(root: &Path, directory: &Path) -> Result<()> {
    let root = tokio::fs::canonicalize(root).await?;
    let directory = tokio::fs::canonicalize(directory).await?;
    if !directory.starts_with(&root) {
        bail!("Plugin path escapes the repository");
    }
    Ok(())
}

async fn private_directory(path: &Path) -> Result<()> {
    let mut builder = tokio::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    builder.mode(0o700);
    builder.create(path).await?;
    let metadata = tokio::fs::symlink_metadata(path).await?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!(
            "Managed plugin directory must be a real directory: {}",
            path.display()
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).await?;
    }
    Ok(())
}

async fn remove_tree(path: &Path) -> Result<()> {
    match tokio::fs::symlink_metadata(path).await {
        Ok(metadata) if metadata.file_type().is_symlink() => tokio::fs::remove_file(path).await?,
        Ok(_) => tokio::fs::remove_dir_all(path).await?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

async fn read_json(path: &Path) -> Result<Value> {
    let file = tokio::fs::File::open(path)
        .await
        .with_context(|| format!("Cannot read {}", path.display()))?;
    let mut bytes = Vec::new();
    file.take(16 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .await?;
    if bytes.len() > 16 * 1024 * 1024 {
        bail!("JSON file exceeds 16 MiB: {}", path.display());
    }
    serde_json::from_slice(&bytes).with_context(|| format!("Invalid JSON in {}", path.display()))
}

async fn atomic_registry(root: &Path, records: &Map<String, Value>) -> Result<()> {
    let path = root.join("sources.json");
    let temporary = root.join(format!(".sources-{}.tmp", Uuid::new_v4()));
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let result = async {
        let mut file = options.open(&temporary).await?;
        let mut bytes = serde_json::to_vec_pretty(records)?;
        bytes.push(b'\n');
        file.write_all(&bytes).await?;
        file.sync_all().await?;
        drop(file);
        tokio::fs::rename(&temporary, &path).await?;
        Ok::<_, anyhow::Error>(())
    }
    .await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(&temporary).await;
    }
    result
}

async fn command(
    program: &str,
    args: Vec<String>,
    cwd: &Path,
    env: BTreeMap<String, String>,
) -> Result<String> {
    let result = crate::process::run(
        CommandSpec {
            program: program.to_owned(),
            args,
            cwd: Some(cwd.to_owned()),
            env,
            clear_env: true,
        },
        1024 * 1024,
        Duration::from_secs(120),
        CancellationToken::new(),
    )
    .await?;
    if result.status != 0 {
        bail!(
            "{program} failed (exit {}): {}\n{}",
            result.status,
            String::from_utf8_lossy(&result.stdout),
            result.stderr
        );
    }
    String::from_utf8(result.stdout).context("Command output is not UTF-8")
}

#[cfg(test)]
mod tests;

pub(crate) fn split_source_reference(source: &str) -> Result<(String, Option<String>)> {
    if reference::registry(source, "https://plugins.paseo.sh")?.is_some() {
        return Ok((source.to_owned(), None));
    }
    Ok(reference::split(source))
}
