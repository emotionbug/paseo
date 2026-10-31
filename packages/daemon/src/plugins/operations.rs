use super::*;

impl PluginService {
    pub async fn handle(&self, context: &RequestContext, message: &Value) -> Result<bool> {
        let kind = message["type"].as_str().unwrap_or("");
        const OWNED: &[&str] = &[
            "plugin.list.request",
            "plugin.catalog.get.request",
            "plugin.logs.get.request",
            "plugin.directory.inspect.request",
            "plugin.directory.install.request",
            "plugin.source.install.request",
            "plugin.source.status.request",
            "plugin.source.update.preview.request",
            "plugin.source.update.apply.request",
            "plugin.source.update.request",
            "plugin.reload.request",
            "plugin.enable.request",
            "plugin.disable.request",
            "plugin.remove.request",
            "plugin.rpc.invoke.request",
        ];
        if !OWNED.contains(&kind) {
            return Ok(false);
        }
        if !context.allows("daemon.manage") {
            bail!("Permission denied: daemon.manage");
        }
        let request_id = message["requestId"]
            .as_str()
            .context("Plugin request requires requestId")?;
        let id = message["pluginId"].as_str().unwrap_or("");
        let mut payload = match kind {
            "plugin.list.request" => json!({"plugins":self.list_plugins().await?}),
            "plugin.catalog.get.request" => json!({"plugins":self.catalog()}),
            "plugin.logs.get.request" => json!({"pluginId":id,"entries":self.get_logs(id)?}),
            "plugin.directory.inspect.request" => {
                let app = self.app.upgrade().context("Daemon is shutting down")?;
                let path = manifest::resolve_directory(text(message, "path")?, &app.user_home)?;
                drop(app);
                let manifest = manifest::read(&path).await?;
                json!({"id":manifest["id"]})
            }
            "plugin.directory.install.request" => {
                json!({"plugin":self.install_directory(text(message,"path")?, message["id"].as_str()).await?})
            }
            "plugin.source.install.request" => {
                json!({"plugin":self.install_source(message).await?})
            }
            "plugin.source.status.request" => {
                json!({"plugins":self.status_sources(message["pluginId"].as_str()).await?})
            }
            "plugin.source.update.preview.request" => {
                json!({"plugins":self.preview_updates(message["pluginId"].as_str(), message.get("target")).await?})
            }
            "plugin.source.update.apply.request" => {
                json!({"plugins":self.apply_updates(message["proposals"].as_array().context("Plugin update requires proposals")?).await?})
            }
            "plugin.source.update.request" => {
                bail!("Update the client to review plugin updates before applying them.")
            }
            "plugin.reload.request" => json!({"plugin":self.reload_plugin(id).await?}),
            "plugin.enable.request" => json!({"plugin":self.set_enabled(id,true).await?}),
            "plugin.disable.request" => json!({"plugin":self.set_enabled(id,false).await?}),
            "plugin.remove.request" => {
                self.remove_plugin(id).await?;
                json!({})
            }
            "plugin.rpc.invoke.request" => {
                json!({"output":self.invoke(id,text(message,"method")?,message.get("input").cloned().unwrap_or(Value::Null),context.cancellation()).await?})
            }
            _ => unreachable!(),
        };
        payload["requestId"] = json!(request_id);
        context.send(json!({"type":kind.strip_suffix("request").unwrap().to_owned()+"response","payload":payload})).await?;
        Ok(true)
    }
    pub fn get_logs(&self, id: &str) -> Result<Vec<Value>> {
        if !is_builtin(id) {
            self.source(id)?;
        }
        let entries: Vec<Value> = self
            .logs
            .lock()
            .unwrap()
            .get(id)
            .map(|tail| tail.entries.iter().cloned().collect())
            .unwrap_or_default();
        Ok(entries)
    }
    async fn patch_source(&self, id: &str, source: Option<Value>) -> Result<()> {
        let app = self.app.upgrade().context("Daemon is shutting down")?;
        let mut plugins = self
            .current
            .read()
            .unwrap()
            .get("plugins")
            .cloned()
            .unwrap_or_else(|| json!({}));
        let sources = plugins
            .as_object_mut()
            .context("Plugin configuration must be an object")?;
        if let Some(source) = source {
            sources.insert(id.to_owned(), source);
        } else {
            sources.remove(id);
        }
        app.config.patch(json!({"plugins":plugins})).await?;
        Ok(())
    }
    async fn refresh_registry(&self) -> Result<()> {
        let app = self.app.upgrade().context("Daemon is shutting down")?;
        let config = self.current.read().unwrap().clone();
        app.providers.apply_config(&config, &config).await
    }
    pub async fn install_directory(&self, path: &str, requested_id: Option<&str>) -> Result<Value> {
        let _operation = self.operations.lock().await;
        let app = self.app.upgrade().context("Daemon is shutting down")?;
        let directory = manifest::resolve_directory(path, &app.user_home)?;
        drop(app);
        let manifest = manifest::read(&directory).await?;
        manifest::check_compatibility(&manifest)?;
        let id = requested_id.unwrap_or(
            manifest["id"]
                .as_str()
                .context("Plugin manifest missing id")?,
        );
        self.require_available_id(id)?;
        self.patch_source(
            id,
            Some(json!({"source":"directory","path":directory,"enabled":true})),
        )
        .await?;
        let item = self.item(id).await?;
        if item["status"] == "failed" {
            bail!(
                "{}",
                item["error"].as_str().unwrap_or("Plugin failed to start")
            );
        }
        Ok(item)
    }
    fn require_available_id(&self, id: &str) -> Result<()> {
        manifest::validate_id(id)?;
        if is_builtin(id) {
            bail!("Plugin ID \"{id}\" is reserved for a built-in plugin");
        }
        if self.current.read().unwrap()["plugins"].get(id).is_some() {
            bail!("Plugin ID \"{id}\" is already configured; choose another ID with --id");
        }
        Ok(())
    }
    pub async fn install_source(&self, input: &Value) -> Result<Value> {
        let _operation = self.operations.lock().await;
        if let Some(id) = input["id"].as_str() {
            self.require_available_id(id)?;
        }
        let managed = self.managed()?;
        let mut candidate = managed.prepare_install(input).await?;
        let id = input["id"]
            .as_str()
            .unwrap_or(&candidate.default_id)
            .to_owned();
        if candidate.record.is_null() {
            self.require_available_id(&id)?;
            let manifest = manifest::read(&candidate.directory).await?;
            manifest::check_compatibility(&manifest)?;
            self.patch_source(
                &id,
                Some(json!({"source":"directory","path":candidate.directory,"enabled":true})),
            )
            .await?;
            let item = self.item(&id).await?;
            if item["status"] == "failed" {
                bail!(
                    "{}",
                    item["error"].as_str().unwrap_or("Plugin failed to start")
                );
            }
            return Ok(item);
        }
        let preparation: Result<()> = async {
            self.require_available_id(&id)?;
            let manifest = manifest::read(&candidate.directory).await?;
            manifest::check_compatibility(&manifest)?;
            managed.build(&candidate).await?;
            Ok(())
        }
        .await;
        if let Err(error) = preparation {
            let _ = managed.discard(&candidate).await;
            return Err(error);
        }
        candidate = managed.place(&id, candidate).await?;
        let validation = async {
            self.validate_candidate(&candidate.directory).await?;
            managed.verify(&id, &candidate, None).await
        }
        .await;
        if let Err(error) = validation {
            let _ = managed.discard(&candidate).await;
            return Err(error);
        }
        let source = json!({"source":"directory","path":candidate.directory,"enabled":true});
        if let Err(error) = self.patch_source(&id, Some(source)).await {
            let _ = managed.discard(&candidate).await;
            return Err(error);
        }
        let activation = async {
            let item = self.item(&id).await?;
            if item["status"] == "failed" {
                bail!(
                    "{}",
                    item["error"].as_str().unwrap_or("Plugin failed to start")
                );
            }
            managed.commit(&id, &candidate.record).await?;
            Ok(item)
        }
        .await;
        if let Err(error) = activation {
            let rollback = self.patch_source(&id, None).await;
            let discarded = managed.discard(&candidate).await;
            self.errors.write().unwrap().remove(&id);
            if rollback.is_err() || discarded.is_err() {
                bail!(
                    "Plugin installation failed: {error}; rollback: {}; cleanup: {}",
                    rollback.err().map(|e| e.to_string()).unwrap_or_default(),
                    discarded.err().map(|e| e.to_string()).unwrap_or_default()
                );
            }
            return Err(error);
        }
        activation
    }
    pub async fn reload_plugin(&self, id: &str) -> Result<Value> {
        let _operation = self.operations.lock().await;
        let source = self.source(id)?;
        if source["enabled"] == false {
            bail!("Plugin is disabled: {id}");
        }
        if self.current.read().unwrap()["pluginsEnabled"] != true {
            bail!("Plugins are globally disabled");
        }
        let result = {
            let _lifecycle = self.lifecycle.lock().await;
            self.stop_plugin(id).await?;
            self.start_plugin(id, &source).await
        };
        if let Err(error) = &result {
            self.record_failure(id, error.to_string());
        }
        self.changed(id).await;
        self.refresh_registry().await?;
        result?;
        self.item(id).await
    }
    pub async fn set_enabled(&self, id: &str, enabled: bool) -> Result<Value> {
        let _operation = self.operations.lock().await;
        let mut source = self.source(id)?;
        let retry =
            enabled && source["enabled"] != false && !self.loaded.read().unwrap().contains_key(id);
        source["enabled"] = json!(enabled);
        self.patch_source(id, Some(source.clone())).await?;
        if retry
            && self.current.read().unwrap()["pluginsEnabled"] == true
            && !self.loaded.read().unwrap().contains_key(id)
        {
            let result = {
                let _lifecycle = self.lifecycle.lock().await;
                self.start_plugin(id, &source).await
            };
            if let Err(error) = result {
                self.record_failure(id, error.to_string());
            }
            self.refresh_registry().await?;
        }
        if !enabled {
            self.errors.write().unwrap().remove(id);
        }
        self.changed(id).await;
        let item = self.item(id).await?;
        if enabled && item["status"] == "failed" {
            bail!(
                "{}",
                item["error"].as_str().unwrap_or("Plugin failed to start")
            );
        }
        Ok(item)
    }
    pub async fn remove_plugin(&self, id: &str) -> Result<()> {
        let _operation = self.operations.lock().await;
        self.source(id)?;
        self.patch_source(id, None).await?;
        self.managed()?.remove(id).await?;
        let app = self.app.upgrade().context("Daemon is shutting down")?;
        let path = app.home.join("plugin-settings").join(id);
        drop(app);
        match tokio::fs::remove_dir_all(path).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        self.errors.write().unwrap().remove(id);
        self.logs.lock().unwrap().remove(id);
        self.changed(id).await;
        Ok(())
    }
    pub async fn status_sources(&self, id: Option<&str>) -> Result<Vec<Value>> {
        let _operation = self.operations.lock().await;
        let selected = self.selected_sources(id)?;
        let mut result = Vec::new();
        for (id, source) in selected {
            result.push(
                self.managed()?
                    .status(&id, Path::new(text(&source, "path")?))
                    .await?,
            );
        }
        Ok(result)
    }
    fn selected_sources(&self, id: Option<&str>) -> Result<Vec<(String, Value)>> {
        if let Some(id) = id {
            return Ok(vec![(id.to_owned(), self.source(id)?)]);
        }
        let mut sources: Vec<_> = self.current.read().unwrap()["plugins"]
            .as_object()
            .map(|sources| {
                sources
                    .iter()
                    .map(|(id, source)| (id.clone(), source.clone()))
                    .collect()
            })
            .unwrap_or_default();
        sources.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(sources)
    }
    pub async fn preview_updates(
        &self,
        id: Option<&str>,
        target: Option<&Value>,
    ) -> Result<Vec<Value>> {
        if target.is_some() && id.is_none() {
            bail!("An explicit update target requires one plugin ID");
        }
        let selected = self.selected_sources(id)?;
        let mut result = Vec::new();
        for (id, source) in selected {
            match self
                .managed()?
                .preview(&id, Path::new(text(&source, "path")?), target)
                .await
            {
                Ok(preview) => result.push(preview),
                Err(error) => result
                    .push(json!({"id":id,"outcome":"error","links":[],"error":error.to_string()})),
            }
        }
        Ok(result)
    }
    pub async fn apply_updates(&self, proposals: &[Value]) -> Result<Vec<Value>> {
        let _operation = self.operations.lock().await;
        let mut results = Vec::new();
        for proposal in proposals {
            match self.update_source(proposal).await {
                Ok(result) => results.push(result),
                Err(error) => results
                    .push(json!({"id":proposal["id"],"outcome":"error","error":error.to_string()})),
            }
        }
        Ok(results)
    }
    async fn update_source(&self, proposal: &Value) -> Result<Value> {
        let id = text(proposal, "id")?;
        let previous = self.source(id)?;
        let old_path = PathBuf::from(text(&previous, "path")?);
        let managed = self.managed()?;
        let candidate = managed.prepare_update(proposal, &old_path).await?;
        let prepared = async {
            let manifest = manifest::read(&candidate.directory).await?;
            manifest::check_compatibility(&manifest)?;
            managed.build(&candidate).await
        }
        .await;
        if let Err(error) = prepared {
            let _ = managed.discard(&candidate).await;
            return Err(error);
        }
        let candidate = managed.place(id, candidate).await?;
        let validation = async {
            self.validate_candidate(&candidate.directory).await?;
            managed
                .verify(id, &candidate, proposal.get("target"))
                .await?;
            managed.assert_current(proposal, &old_path).await
        }
        .await;
        if let Err(error) = validation {
            let _ = managed.discard(&candidate).await;
            return Err(error);
        }
        let mut next = previous.clone();
        next["path"] = json!(candidate.directory);
        let activation = async {
            self.patch_source(id, Some(next)).await?;
            let item = self.item(id).await?;
            if item["status"] == "failed" {
                bail!(
                    "{}",
                    item["error"]
                        .as_str()
                        .unwrap_or("Plugin update failed to start")
                );
            }
            managed.commit(id, &candidate.record).await?;
            Ok(item)
        }
        .await;
        let item = match activation {
            Ok(item) => item,
            Err(error) => {
                let restored = self.patch_source(id, Some(previous)).await;
                let cleanup = managed.discard(&candidate).await;
                if let Err(restore_error) = restored {
                    bail!(
                        "Update failed: {error}; restoring previous plugin also failed: {restore_error}"
                    );
                }
                if let Err(cleanup_error) = cleanup {
                    bail!("Update failed: {error}; cleanup failed: {cleanup_error}");
                }
                return Err(error);
            }
        };
        let mut result = json!({"id":id,"outcome":"updated","plugin":item});
        if let Err(error) = managed.remove_version(id, &old_path).await {
            result["warning"] = json!(format!(
                "Plugin updated, but previous installation cleanup failed: {error}"
            ));
        }
        Ok(result)
    }
    async fn validate_candidate(&self, directory: &Path) -> Result<()> {
        manifest::require_server_entry(directory).await?;
        let app = self.app.upgrade().context("Daemon is shutting down")?;
        let env = app.config.environment();
        let executable = app.executable.clone();
        drop(app);
        worker::validate(directory, &executable, env, self.stopping.clone()).await
    }
}
fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .with_context(|| format!("Plugin request requires {key}"))
}
