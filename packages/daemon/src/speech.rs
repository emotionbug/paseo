pub mod audio;
pub mod cloud;
mod dictation;
pub mod models;
pub mod native;
mod session;
mod session_audio;

use crate::{state::AppState, transport::RequestContext};
use anyhow::{Result, anyhow, bail};
use bytes::Bytes;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, Weak},
    time::{Duration, Instant},
};
use tokio::sync::{Mutex, OnceCell, RwLock, watch};
use tokio_util::sync::CancellationToken;

struct Engines {
    library: Option<Arc<native::SherpaLibrary>>,
    recognizers: BTreeMap<String, Arc<native::Parakeet>>,
    tts: Option<Arc<native::Kokoro>>,
    validated: std::collections::BTreeSet<String>,
    last_used: Instant,
}
impl Default for Engines {
    fn default() -> Self {
        Self {
            library: None,
            recognizers: BTreeMap::new(),
            tts: None,
            validated: std::collections::BTreeSet::new(),
            last_used: Instant::now(),
        }
    }
}

/// Speech resources are owned by the daemon; conversations by the physical source.
pub struct SpeechService {
    app: Weak<AppState>,
    engines: Mutex<Engines>,
    sources: Mutex<HashMap<String, Arc<session::VoiceSource>>>,
    voice_owners: Mutex<HashMap<String, String>>,
    readiness: watch::Sender<Value>,
    config: RwLock<Value>,
    download: Mutex<(bool, Option<String>)>,
    model_store: OnceCell<Arc<models::ModelStore>>,
    cloud: OnceCell<cloud::CloudSpeech>,
    download_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    cancel: CancellationToken,
    monitor: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl SpeechService {
    pub fn new(app: Weak<AppState>) -> Self {
        Self {
            app,
            engines: Mutex::new(Engines::default()),
            sources: Mutex::new(HashMap::new()),
            voice_owners: Mutex::new(HashMap::new()),
            readiness: watch::channel(Value::Null).0,
            config: RwLock::new(Value::Null),
            download: Mutex::new((false, None)),
            model_store: OnceCell::new(),
            cloud: OnceCell::new(),
            download_task: Mutex::new(None),
            cancel: CancellationToken::new(),
            monitor: Mutex::new(None),
        }
    }
    fn app(&self) -> Result<Arc<AppState>> {
        self.app
            .upgrade()
            .ok_or_else(|| anyhow!("Speech service is shut down"))
    }
    pub async fn initialize(&self) -> Result<()> {
        let app = self.app()?;
        let config = app.config.snapshot();
        let cloud = cloud::CloudSpeech::from_resolved(&config["openai"])?;
        let missing = cloud.missing_credential_features(&config["speech"]["providers"]);
        if !missing.is_empty() {
            tracing::warn!(features=?missing, "OpenAI speech credentials missing; selected features are unavailable");
        }
        self.cloud
            .set(cloud)
            .map_err(|_| anyhow!("Speech service already initialized"))?;
        self.model_store
            .set(Arc::new(models::ModelStore::new(self.models_dir(&config)?)))
            .map_err(|_| anyhow!("Speech model store already initialized"))?;
        *self.config.write().await = config;
        self.reconcile().await?;
        let weak = self.app.clone();
        let cancel = self.cancel.clone();
        *self.monitor.lock().await = Some(tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(3));
            loop {
                tokio::select! { _ = cancel.cancelled() => break, _ = interval.tick() => {} }
                let Some(app) = weak.upgrade() else { break };
                if let Err(error) = app.speech.reconcile().await {
                    tracing::warn!(%error,"Speech reconciliation failed");
                }
                app.speech.unload_idle().await;
            }
        }));
        Ok(())
    }
    fn selection<'a>(config: &'a Value, feature: &str) -> (&'a str, bool) {
        let value = &config["speech"]["providers"][feature];
        (
            value["provider"].as_str().unwrap_or("local"),
            value["enabled"].as_bool().unwrap_or(true),
        )
    }
    fn local_model(config: &Value, feature: &str) -> String {
        config["speech"]["local"]["models"][feature]
            .as_str()
            .unwrap_or(if feature == "voiceTts" {
                "kokoro-en-v0_19"
            } else {
                "parakeet-tdt-0.6b-v2-int8"
            })
            .to_owned()
    }
    fn models_dir(&self, config: &Value) -> Result<std::path::PathBuf> {
        Ok(config["speech"]["local"]["modelsDir"]
            .as_str()
            .map(Into::into)
            .unwrap_or(self.app()?.home.join("models/local-speech")))
    }
    async fn library(&self) -> Result<Arc<native::SherpaLibrary>> {
        let mut engines = self.engines.lock().await;
        if let Some(library) = &engines.library {
            return Ok(library.clone());
        }
        let env = self.app()?.config.environment();
        let path = env
            .get("PASEO_SHERPA_ONNX_LIBRARY")
            .map(std::path::PathBuf::from);
        let library =
            tokio::task::spawn_blocking(move || native::SherpaLibrary::load(path.as_deref()))
                .await??;
        engines.library = Some(library.clone());
        Ok(library)
    }
    pub async fn required_sample_rate(&self, dictation: bool) -> Result<u32> {
        let config = self.config.read().await.clone();
        let feature = if dictation {
            "dictationStt"
        } else {
            "voiceStt"
        };
        let (provider, enabled) = Self::selection(&config, feature);
        if !enabled {
            bail!("Speech-to-text is disabled")
        }
        match provider {
            "openai" => {
                if !self
                    .cloud
                    .get()
                    .is_some_and(|cloud| cloud.availability().stt)
                {
                    bail!("OpenAI speech-to-text credentials are missing")
                }
                Ok(24000)
            }
            "local" => {
                self.recognizer(&config, feature).await?;
                Ok(16000)
            }
            other => bail!("Unknown speech provider: {other}"),
        }
    }
    pub async fn is_local_stt(&self, dictation: bool) -> bool {
        Self::selection(
            &*self.config.read().await,
            if dictation {
                "dictationStt"
            } else {
                "voiceStt"
            },
        )
        .0 == "local"
    }
    async fn recognizer(&self, config: &Value, feature: &str) -> Result<Arc<native::Parakeet>> {
        let id = Self::local_model(config, feature);
        let store = self
            .model_store
            .get()
            .ok_or_else(|| anyhow!("Speech service is not initialized"))?;
        if !store.is_installed_cancellable(&id, &self.cancel).await? {
            bail!(
                "Local speech model {id} is missing or corrupt; complete the model download before using speech"
            )
        }
        let library = self.library().await?;
        let mut engines = self.engines.lock().await;
        engines.last_used = Instant::now();
        if let Some(recognizer) = engines.recognizers.get(&id) {
            return Ok(recognizer.clone());
        }
        let dir = models::model_dir(&self.models_dir(config)?, &id)?;
        let recognizer = library
            .create_parakeet(native::ParakeetConfig::from_model_dir(dir))
            .await?;
        engines.recognizers.insert(id, recognizer.clone());
        Ok(recognizer)
    }
    pub async fn transcribe(
        &self,
        dictation: bool,
        bytes: Bytes,
        format: &str,
        cancel: &CancellationToken,
    ) -> Result<Value> {
        let started = Instant::now();
        let config = self.config.read().await.clone();
        let feature = if dictation {
            "dictationStt"
        } else {
            "voiceStt"
        };
        let (provider, enabled) = Self::selection(&config, feature);
        if !enabled {
            bail!("Speech-to-text is disabled")
        }
        let language = config["speech"]["sttLanguages"]
            [if dictation { "dictation" } else { "voice" }]
        .as_str()
        .unwrap_or("en");
        if provider == "openai" {
            let (bytes, format) = if format.to_ascii_lowercase().contains("pcm") {
                let rate = audio::parse_pcm_rate_from_format(format, Some(24000))
                    .ok_or_else(|| anyhow!("Invalid PCM sample rate"))?;
                let pcm;
                let samples = if rate != 24000 {
                    pcm = audio::Pcm16MonoResampler::new(rate, 24000)?.process_chunk(&bytes)?;
                    pcm.as_slice()
                } else {
                    bytes.as_ref()
                };
                (
                    Bytes::from(audio::pcm16_mono_wav(samples, 24000)?),
                    "audio/wav",
                )
            } else {
                (bytes, format)
            };
            let environment = self.app()?.config.environment();
            let prompt = dictation.then(||environment.get("PASEO_DICTATION_TRANSCRIPTION_PROMPT").map(String::as_str).unwrap_or("Transcribe only what the speaker says. Do not add words. Preserve punctuation and casing. If the audio is silence or non-speech noise, return an empty transcript."));
            return Ok(serde_json::to_value(
                self.cloud
                    .get()
                    .ok_or_else(|| anyhow!("Speech service is not initialized"))?
                    .transcribe(bytes, format, language, prompt, cancel)
                    .await?,
            )?);
        }
        if provider != "local" {
            bail!("Unknown speech provider: {provider}")
        }
        let recognizer = self.recognizer(&config, feature).await?;
        let (rate, pcm) = if format.to_ascii_lowercase().contains("pcm") {
            (
                audio::parse_pcm_rate_from_format(format, Some(16000))
                    .ok_or_else(|| anyhow!("Invalid PCM sample rate"))?,
                bytes.as_ref(),
            )
        } else {
            let wav = audio::parse_pcm16_mono_wav(&bytes)?;
            (wav.sample_rate, wav.pcm16)
        };
        let peak = audio::pcm16le_peak_abs(pcm)?;
        if !dictation && peak < 300 {
            return Ok(
                json!({"text":"","duration":started.elapsed().as_millis() as u64,"isLowConfidence":true}),
            );
        }
        let resampled;
        let pcm = if rate != 16000 {
            resampled = audio::Pcm16MonoResampler::new(rate, 16000)?.process_chunk(pcm)?;
            resampled.as_slice()
        } else {
            pcm
        };
        let peak = audio::pcm16le_peak_abs(pcm)? as f64 / 32768.0;
        let gain = if peak > 0.0 && peak < 0.6 {
            (0.6 / peak).min(50.0)
        } else {
            1.0
        };
        let samples = audio::pcm16le_to_float32(pcm, gain)?;
        let token = cancel.child_token();
        let _cancel_on_drop = token.clone().drop_guard();
        let text = tokio::time::timeout(
            Duration::from_secs(30),
            recognizer.transcribe(samples, 16000, token),
        )
        .await
        .map_err(|_| anyhow!("Local speech worker request timed out after 30000ms"))??;
        Ok(json!({"text":text,"language":language,"duration":started.elapsed().as_millis() as u64}))
    }
    async fn tts(&self, config: &Value) -> Result<Arc<native::Kokoro>> {
        let id = Self::local_model(config, "voiceTts");
        let store = self
            .model_store
            .get()
            .ok_or_else(|| anyhow!("Speech service is not initialized"))?;
        if !store.is_installed_cancellable(&id, &self.cancel).await? {
            bail!(
                "Local speech model {id} is missing or corrupt; complete the model download before using speech"
            )
        }
        let library = self.library().await?;
        let mut engines = self.engines.lock().await;
        engines.last_used = Instant::now();
        if let Some(tts) = &engines.tts {
            return Ok(tts.clone());
        }
        let dir = models::model_dir(&self.models_dir(config)?, &id)?;
        let mut native_config = native::KokoroConfig::from_model_dir(dir);
        if let Some(id) = config["speech"]["local"]["models"]["voiceTtsSpeakerId"].as_i64() {
            native_config.speaker_id = i32::try_from(id)?;
        }
        if let Some(speed) = config["speech"]["local"]["models"]["voiceTtsSpeed"].as_f64() {
            native_config.speed = speed as f32;
        }
        let tts = library.create_kokoro(native_config).await?;
        engines.tts = Some(tts.clone());
        Ok(tts)
    }
    pub async fn synthesize(
        &self,
        text: &str,
        cancel: &CancellationToken,
    ) -> Result<cloud::SpeechAudio> {
        let config = self.config.read().await.clone();
        let (provider, enabled) = Self::selection(&config, "voiceTts");
        // Source TTSManager: a disabled feature or an OpenAI selection without
        // credentials resolves no provider.
        if !enabled {
            bail!("TTS not configured")
        }
        if provider == "openai" {
            let cloud = self
                .cloud
                .get()
                .ok_or_else(|| anyhow!("Speech service is not initialized"))?;
            if !cloud.availability().tts {
                bail!("TTS not configured")
            }
            return cloud.synthesize(text, cancel).await;
        }
        if provider != "local" {
            bail!("Unknown speech provider: {provider}")
        }
        let tts = self.tts(&config).await?;
        let token = cancel.child_token();
        let _cancel_on_drop = token.clone().drop_guard();
        let audio = tokio::time::timeout(
            Duration::from_secs(30),
            tts.synthesize(text.to_owned(), token),
        )
        .await
        .map_err(|_| anyhow!("Local speech worker request timed out: tts.synthesize"))??;
        Ok(cloud::SpeechAudio {
            bytes: Bytes::from(audio::float32_to_pcm16le(&audio.samples)),
            format: format!("pcm;rate={}", audio.sample_rate),
        })
    }
    async fn create_vad(&self) -> Result<Arc<native::Silero>> {
        self.library()
            .await?
            .create_silero(native::SileroConfig::default())
            .await
    }
    pub async fn readiness(&self) -> Value {
        self.readiness.borrow().clone()
    }
    pub fn subscribe_readiness(&self) -> watch::Receiver<Value> {
        self.readiness.subscribe()
    }
    async fn reconcile(&self) -> Result<()> {
        let config = self.config.read().await.clone();
        let mut required = Vec::new();
        for feature in ["dictationStt", "voiceStt", "voiceTts"] {
            if Self::selection(&config, feature) == ("local", true) {
                let id = Self::local_model(&config, feature);
                if !required.contains(&id) {
                    required.push(id);
                }
            }
        }
        let store = self
            .model_store
            .get()
            .ok_or_else(|| anyhow!("Speech service is not initialized"))?
            .clone();
        let missing = store.missing_models(&required).await?;
        let mut download = self.download.lock().await;
        if !missing.is_empty() && !download.0 && download.1.is_none() {
            download.0 = true;
            let weak = self.app.clone();
            let ids = missing.clone();
            let cancel = self.cancel.clone();
            *self.download_task.lock().await = Some(tokio::spawn(async move {
                let result = store.ensure_models(&ids, &cancel).await;
                if let Some(app) = weak.upgrade() {
                    let mut state = app.speech.download.lock().await;
                    state.0 = false;
                    state.1 = result.err().map(|e| e.to_string());
                }
            }));
        }
        let download_state = download.clone();
        drop(download);
        let needs_native = ["dictationStt", "voiceStt", "voiceTts", "voiceTurnDetection"]
            .iter()
            .any(|f| Self::selection(&config, f) == ("local", true));
        let native_error = if needs_native {
            self.library().await.err().map(|e| e.to_string())
        } else {
            None
        };
        let mut load_errors = BTreeMap::new();
        if native_error.is_none() {
            for feature in ["dictationStt", "voiceStt", "voiceTts", "voiceTurnDetection"] {
                if Self::selection(&config, feature) != ("local", true) {
                    continue;
                }
                if feature != "voiceTurnDetection"
                    && missing.contains(&Self::local_model(&config, feature))
                {
                    self.engines.lock().await.validated.remove(feature);
                    continue;
                }
                if self.engines.lock().await.validated.contains(feature) {
                    continue;
                }
                let loaded = match feature {
                    "voiceTurnDetection" => match self.create_vad().await {
                        Ok(vad) => vad.close().await,
                        Err(error) => Err(error),
                    },
                    "voiceTts" => self.tts(&config).await.map(|_| ()),
                    _ => self.recognizer(&config, feature).await.map(|_| ()),
                };
                match loaded {
                    Ok(()) => {
                        self.engines
                            .lock()
                            .await
                            .validated
                            .insert(feature.to_owned());
                    }
                    Err(error) => {
                        load_errors.insert(feature, error.to_string());
                    }
                }
            }
        }
        let cloud = self
            .cloud
            .get()
            .ok_or_else(|| anyhow!("Speech service is not initialized"))?
            .availability();
        let mut availability = BTreeMap::new();
        for feature in ["dictationStt", "voiceStt", "voiceTts", "voiceTurnDetection"] {
            let (provider, enabled) = Self::selection(&config, feature);
            let available = enabled
                && match provider {
                    "local" => {
                        native_error.is_none()
                            && !load_errors.contains_key(feature)
                            && (feature == "voiceTurnDetection"
                                || !missing.contains(&Self::local_model(&config, feature)))
                    }
                    "openai" if feature != "voiceTurnDetection" => {
                        if feature == "voiceTts" {
                            cloud.tts
                        } else {
                            cloud.stt
                        }
                    }
                    _ => false,
                };
            availability.insert(feature, (enabled, available));
        }
        let state = |enabled: bool,
                     available: bool,
                     code: &str,
                     message: String,
                     retryable: bool,
                     ids: Vec<String>| json!({"enabled":enabled,"available":available,"reasonCode":code,"message":message,"retryable":retryable,"missingModelIds":ids});
        let dictation = if !availability["dictationStt"].0 {
            state(
                false,
                false,
                "disabled",
                "Dictation is disabled in daemon config.".into(),
                false,
                vec![],
            )
        } else if availability["dictationStt"].1 {
            state(
                true,
                true,
                "ready",
                "Dictation is ready.".into(),
                false,
                vec![],
            )
        } else {
            state(
                true,
                false,
                "stt_unavailable",
                load_errors
                    .get("dictationStt")
                    .cloned()
                    .or_else(|| native_error.clone())
                    .unwrap_or(
                        "Dictation is unavailable: speech-to-text service is not ready.".into(),
                    ),
                false,
                vec![],
            )
        };
        let voice_enabled = ["voiceTurnDetection", "voiceStt", "voiceTts"]
            .iter()
            .any(|f| availability[f].0);
        let failed = ["voiceTurnDetection", "voiceStt", "voiceTts"]
            .into_iter()
            .find(|f| availability[f].0 && !availability[f].1);
        let voice = if !voice_enabled {
            state(
                false,
                false,
                "disabled",
                "Realtime voice is disabled in daemon config.".into(),
                false,
                vec![],
            )
        } else if let Some(f) = failed {
            let (code, label) = match f {
                "voiceTurnDetection" => ("turn_detection_unavailable", "turn-detection"),
                "voiceStt" => ("stt_unavailable", "speech-to-text"),
                _ => ("tts_unavailable", "text-to-speech"),
            };
            state(
                true,
                false,
                code,
                load_errors
                    .get(f)
                    .cloned()
                    .or_else(|| native_error.clone())
                    .unwrap_or(format!(
                        "Realtime voice is unavailable: {label} service is not ready."
                    )),
                false,
                vec![],
            )
        } else {
            state(
                true,
                true,
                "ready",
                "Realtime voice is ready.".into(),
                false,
                vec![],
            )
        };
        let enabled = voice_enabled || availability["dictationStt"].0;
        let feature = if !enabled {
            state(
                false,
                false,
                "disabled",
                "Voice features are disabled in daemon config.".into(),
                false,
                vec![],
            )
        } else if !missing.is_empty() {
            let ids = missing.join(", ");
            let (code, message, retryable) = if download_state.0 {
                (
                    "model_download_in_progress",
                    format!(
                        "Voice features are unavailable while models download in the background ({ids})."
                    ),
                    true,
                )
            } else if let Some(error) = &download_state.1 {
                (
                    "model_download_failed",
                    format!("Voice features are unavailable: model download failed ({error})."),
                    false,
                )
            } else {
                (
                    "models_missing",
                    format!("Voice features are unavailable: missing local models ({ids})."),
                    true,
                )
            };
            state(true, false, code, message, retryable, missing.clone())
        } else {
            state(
                true,
                true,
                "ready",
                "Voice features are ready.".into(),
                false,
                vec![],
            )
        };
        let snapshot = json!({"generatedAt":chrono::Utc::now().to_rfc3339(),"requiredLocalModelIds":required,"missingLocalModelIds":missing,"download":{"inProgress":download_state.0,"error":download_state.1},"realtimeVoice":voice,"dictation":dictation,"voiceFeature":feature});
        let changed = {
            let previous = self.readiness.borrow();
            [
                "requiredLocalModelIds",
                "missingLocalModelIds",
                "download",
                "realtimeVoice",
                "dictation",
                "voiceFeature",
            ]
            .iter()
            .any(|key| previous[*key] != snapshot[*key])
        };
        if changed {
            self.readiness.send_replace(snapshot);
        }
        Ok(())
    }
    async fn unload_idle(&self) {
        let sources: Vec<_> = self
            .sources
            .lock()
            .await
            .iter()
            .map(|(id, source)| (id.clone(), source.clone()))
            .collect();
        let mut active = false;
        for (id, source) in sources {
            if source.has_demand().await {
                active = true;
                continue;
            }
            let removed = {
                let mut sources = self.sources.lock().await;
                if Arc::strong_count(&source) == 2 {
                    sources.remove(&id)
                } else {
                    active = true;
                    None
                }
            };
            if let Some(source) = removed {
                if let Err(error) = source.shutdown().await {
                    tracing::warn!(%error,"Idle speech source cleanup failed");
                }
            }
        }
        if !active {
            let mut engines = self.engines.lock().await;
            if engines.last_used.elapsed() >= Duration::from_secs(300) {
                engines.recognizers.clear();
                engines.tts = None;
            }
        }
    }
    pub async fn handle(&self, context: &RequestContext, message: &Value) -> Result<bool> {
        if !matches!(
            message["type"].as_str(),
            Some(
                "set_voice_mode"
                    | "voice_audio_chunk"
                    | "audio_played"
                    | "abort_request"
                    | "dictation_stream_start"
                    | "dictation_stream_chunk"
                    | "dictation_stream_finish"
                    | "dictation_stream_cancel"
            )
        ) {
            return Ok(false);
        }
        let source = {
            let mut sources = self.sources.lock().await;
            if let Some(source) = sources.get(context.source_id()) {
                source.clone()
            } else {
                let source = session::VoiceSource::new(self.app.clone(), context.clone())?;
                sources.insert(context.source_id().to_owned(), source.clone());
                source
            }
        };
        source.handle(message).await?;
        Ok(true)
    }
    pub async fn detach_source(&self, source_id: &str) -> Result<()> {
        let source = self.sources.lock().await.remove(source_id);
        if let Some(source) = source {
            source.shutdown().await?;
        }
        Ok(())
    }
    pub async fn is_active_for_agent(&self, agent_id: &str) -> bool {
        let sources: Vec<_> = self.sources.lock().await.values().cloned().collect();
        for source in sources {
            if source.is_active_for_agent(agent_id).await {
                return true;
            }
        }
        false
    }
    /// Mirrors the source registerVoiceSpeakHandler: one speak handler per agent.
    async fn register_voice_owner(&self, agent_id: &str, source_id: &str) -> Result<()> {
        let mut owners = self.voice_owners.lock().await;
        if owners.get(agent_id).is_some_and(|owner| owner != source_id) {
            bail!("Voice mode is already active for this agent");
        }
        owners.insert(agent_id.to_owned(), source_id.to_owned());
        Ok(())
    }
    async fn unregister_voice_owner(&self, agent_id: &str, source_id: &str) {
        let mut owners = self.voice_owners.lock().await;
        if owners.get(agent_id).is_some_and(|owner| owner == source_id) {
            owners.remove(agent_id);
        }
    }
    pub async fn speak(
        &self,
        agent_id: &str,
        text: &str,
        cancel: CancellationToken,
    ) -> Result<Value> {
        let owner = self.voice_owners.lock().await.get(agent_id).cloned();
        let source = if let Some(owner) = owner {
            self.sources.lock().await.get(&owner).cloned()
        } else {
            None
        };
        if let Some(source) = source {
            if source.is_active_for_agent(agent_id).await {
                source.speak(text, &cancel).await?;
                return Ok(json!({"status":"spoken"}));
            }
        }
        Err(crate::mcp::input_error(format!(
            "No speak handler registered for your session '{agent_id}'"
        )))
    }
    pub async fn shutdown(&self) -> Result<()> {
        self.cancel.cancel();
        if let Some(task) = self.monitor.lock().await.take() {
            task.await?;
        }
        if let Some(task) = self.download_task.lock().await.take() {
            task.await?;
        }
        let sources = std::mem::take(&mut *self.sources.lock().await);
        for (_, source) in sources {
            if let Err(error) = source.shutdown().await {
                tracing::warn!(%error,"Speech source shutdown failed");
            }
        }
        *self.engines.lock().await = Engines::default();
        Ok(())
    }
}

pub fn is_voice_permission_allowed(request: &Value) -> bool {
    if request["kind"] != "tool" {
        return false;
    }
    let name = request["name"]
        .as_str()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    name.split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|token| !token.is_empty())
        .next_back()
        == Some("speak")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn voice_speak_handler_belongs_to_one_source_per_agent() {
        let speech = SpeechService::new(Weak::new());
        speech.register_voice_owner("agent", "first").await.unwrap();
        speech.register_voice_owner("agent", "first").await.unwrap();
        assert_eq!(
            speech
                .register_voice_owner("agent", "second")
                .await
                .unwrap_err()
                .to_string(),
            "Voice mode is already active for this agent"
        );
        speech.unregister_voice_owner("agent", "second").await;
        assert!(speech.register_voice_owner("agent", "second").await.is_err());
        speech.unregister_voice_owner("agent", "first").await;
        speech.register_voice_owner("agent", "second").await.unwrap();
    }

    #[tokio::test]
    async fn speak_without_a_voice_handler_reports_the_source_input_error() {
        let speech = SpeechService::new(Weak::new());
        let error = speech
            .speak("agent", "hello", CancellationToken::new())
            .await
            .unwrap_err();
        assert!(error.is::<crate::providers::api::NativeToolInputError>());
        assert_eq!(
            error.to_string(),
            "No speak handler registered for your session 'agent'"
        );
    }
}
