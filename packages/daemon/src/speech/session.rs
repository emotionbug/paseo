use super::{
    audio::{self, Pcm16MonoResampler},
    dictation::DictationManager,
    native::Silero,
};
use crate::{
    state::AppState,
    transport::{OwnedOperation, RequestContext},
};
use anyhow::{Result, anyhow, bail};
use base64::{Engine, engine::general_purpose::STANDARD};
use bytes::Bytes;
use serde_json::{Value, json};
use std::{
    collections::{HashMap, VecDeque},
    sync::{
        Arc, Weak,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};
use tokio::sync::{Mutex, oneshot};
use tokio_util::sync::CancellationToken;

const OUTPUTS: &[&str] = &[
    "audio_output",
    "transcription_result",
    "voice_input_state",
    "set_voice_mode_response",
    "dictation_stream_ack",
    "dictation_stream_partial",
    "dictation_stream_final",
    "dictation_stream_error",
    "dictation_stream_finish_accepted",
    "activity_log",
];
const MAX_INPUT: usize = 16_000 * 2 * 300;
const MAX_TTS_SEGMENT_CHARS: usize = 260;
const TTS_PREFETCH_SEGMENTS: usize = 2;

#[derive(Default)]
enum Phase {
    #[default]
    Idle,
    Confirming(u64),
    Speaking,
    Ending(u64),
}
struct Input {
    vad: Option<Arc<Silero>>,
    phase: Phase,
    window_ms: u64,
    rate: u32,
    resampler: Option<Pcm16MonoResampler>,
    pcm: Vec<u8>,
    buffered: Vec<u8>,
    buffered_format: String,
    started: Option<Instant>,
}
impl Default for Input {
    fn default() -> Self {
        Self {
            vad: None,
            phase: Phase::Idle,
            window_ms: 0,
            rate: 16000,
            resampler: None,
            pcm: vec![],
            buffered: vec![],
            buffered_format: String::new(),
            started: None,
        }
    }
}
struct AgentBinding {
    id: String,
    base_prompt: Option<String>,
}

pub(super) struct VoiceSource {
    app: Weak<AppState>,
    context: RequestContext,
    owner: OwnedOperation,
    dictation: DictationManager,
    binding: Mutex<Option<AgentBinding>>,
    input: Mutex<Input>,
    lifecycle: Mutex<()>,
    playback: Mutex<HashMap<String, oneshot::Sender<()>>>,
    output_cancel: Mutex<CancellationToken>,
    turn: AtomicU64,
    transcriptions: Mutex<Vec<tokio::task::JoinHandle<()>>>,
    transcription_serial: Mutex<()>,
}
impl VoiceSource {
    pub fn new(app: Weak<AppState>, context: RequestContext) -> Result<Arc<Self>> {
        let owner = context.retain_operation(OUTPUTS)?;
        Ok(Arc::new(Self {
            dictation: DictationManager::new(
                app.clone(),
                owner.clone(),
                context.session_id().to_owned(),
            ),
            app,
            context,
            owner,
            binding: Mutex::new(None),
            input: Mutex::new(Input::default()),
            lifecycle: Mutex::new(()),
            playback: Mutex::new(HashMap::new()),
            output_cancel: Mutex::new(CancellationToken::new()),
            turn: AtomicU64::new(0),
            transcriptions: Mutex::new(vec![]),
            transcription_serial: Mutex::new(()),
        }))
    }
    fn app(&self) -> Result<Arc<AppState>> {
        self.app
            .upgrade()
            .ok_or_else(|| anyhow!("Voice service is shut down"))
    }
    async fn emit(&self, message: Value) -> Result<()> {
        self.owner.emit(message).await
    }
    async fn log(&self, kind: &str, text: &str) -> Result<()> {
        self.emit(json!({"type":"activity_log","payload":{"id":uuid::Uuid::new_v4().to_string(),"timestamp":chrono::Utc::now().to_rfc3339(),"type":kind,"content":text}})).await
    }
    pub async fn is_active_for_agent(&self, id: &str) -> bool {
        !self.owner.cancellation().is_cancelled()
            && self
                .binding
                .lock()
                .await
                .as_ref()
                .is_some_and(|b| b.id == id)
    }
    pub async fn has_demand(&self) -> bool {
        if self.binding.lock().await.is_some()
            || self.dictation.has_demand().await
            || !self.playback.lock().await.is_empty()
        {
            return true;
        }
        if self
            .transcriptions
            .lock()
            .await
            .iter()
            .any(|task| !task.is_finished())
        {
            return true;
        }
        let input = self.input.lock().await;
        !input.buffered.is_empty() || !input.pcm.is_empty()
    }
    pub async fn handle(self: &Arc<Self>, message: &Value) -> Result<()> {
        if message["type"] == "dictation_stream_start" {
            let readiness = self.app()?.speech.readiness().await;
            let mode = &readiness["dictation"];
            let unavailable = if mode["enabled"] == false {
                Some(mode)
            } else if readiness["voiceFeature"]["available"] == false {
                Some(&readiness["voiceFeature"])
            } else if mode["available"] == false {
                Some(mode)
            } else {
                None
            };
            if let Some(state) = unavailable {
                return self.emit(json!({"type":"dictation_stream_error","payload":{"dictationId":message["dictationId"],"error":state["message"],"retryable":state["retryable"],"reasonCode":state["reasonCode"],"missingModelIds":state["missingModelIds"]}})).await;
            }
        }
        if self.dictation.handle(message).await? {
            return Ok(());
        }
        match message["type"].as_str().unwrap_or("") {
            "set_voice_mode" => self.set_mode(message).await,
            "audio_played" => {
                let id = message["id"]
                    .as_str()
                    .unwrap_or("")
                    .split(':')
                    .next()
                    .unwrap_or("");
                if let Some(tx) = self.playback.lock().await.remove(id) {
                    let _ = tx.send(());
                }
                Ok(())
            }
            "abort_request" => self.abort().await,
            "voice_audio_chunk" => self.audio(message).await,
            _ => Ok(()),
        }
    }
    async fn set_mode(&self, message: &Value) -> Result<()> {
        let _serial = self.lifecycle.lock().await;
        let enabled = message["enabled"]
            .as_bool()
            .ok_or_else(|| anyhow!("enabled must be boolean"))?;
        let result = self.change_mode(enabled, message["agentId"].as_str()).await;
        let binding = self.binding.lock().await;
        let mut payload = json!({"requestId":message["requestId"],"enabled":binding.is_some(),"agentId":binding.as_ref().map(|b|b.id.as_str()),"accepted":result.is_ok(),"error":result.as_ref().err().map(ToString::to_string)});
        if enabled && result.is_err() {
            let readiness = self.app()?.speech.readiness().await;
            let state = if readiness["voiceFeature"]["available"] == false {
                &readiness["voiceFeature"]
            } else {
                &readiness["realtimeVoice"]
            };
            if state["available"] == false {
                for key in ["reasonCode", "retryable", "missingModelIds"] {
                    payload[key] = state[key].clone();
                }
            }
        }
        drop(binding);
        if message["requestId"].is_string() {
            self.emit(json!({"type":"set_voice_mode_response","payload":payload}))
                .await
        } else {
            result
        }
    }
    async fn change_mode(&self, enabled: bool, id: Option<&str>) -> Result<()> {
        if !enabled {
            return self.disable().await;
        }
        let app = self.app()?;
        let readiness = app.speech.readiness().await;
        for field in ["voiceFeature", "realtimeVoice"] {
            if readiness[field]["available"] == false {
                bail!(
                    "{}",
                    readiness[field]["message"]
                        .as_str()
                        .unwrap_or("Voice is unavailable")
                )
            }
        }
        let id = id.unwrap_or("").trim();
        uuid::Uuid::parse_str(id).map_err(|_| anyhow!("set_voice_mode: agentId must be a UUID"))?;
        if self.is_active_for_agent(id).await {
            return Ok(());
        }
        self.disable().await?;
        app.speech.required_sample_rate(false).await?;
        let snapshot = app
            .agents
            .agent_snapshot(id)
            .await?
            .ok_or_else(|| anyhow!("Agent {id} not found"))?;
        // Source order: load the agent, register the per-agent speak handler, then reload.
        app.speech
            .register_voice_owner(id, self.context.source_id())
            .await?;
        let base_prompt = strip_voice_prompt(snapshot["config"]["systemPrompt"].as_str());
        let prompt = voice_prompt(base_prompt.as_deref(), true);
        *self.binding.lock().await = Some(AgentBinding {
            id: id.to_owned(),
            base_prompt,
        });
        let result = app
            .agents
            .control(
                self.context.principal(),
                id,
                "reload",
                json!({"overrides":{"systemPrompt":prompt}}),
            )
            .await;
        if let Err(error) = result {
            self.binding.lock().await.take();
            app.speech
                .unregister_voice_owner(id, self.context.source_id())
                .await;
            return Err(error);
        }
        let vad = match app.speech.create_vad().await {
            Ok(vad) => vad,
            Err(error) => {
                self.disable().await?;
                return Err(error);
            }
        };
        if self.owner.cancellation().is_cancelled() {
            self.disable().await?;
            bail!("Voice source is closed")
        }
        self.input.lock().await.vad = Some(vad);
        Ok(())
    }
    async fn disable(&self) -> Result<()> {
        self.turn.fetch_add(1, Ordering::SeqCst);
        self.renew_output().await;
        let old = std::mem::take(&mut *self.input.lock().await);
        if let Some(vad) = old.vad {
            vad.close().await?;
        }
        let binding = self.binding.lock().await.take();
        if let Some(binding) = binding {
            if let Some(app) = self.app.upgrade() {
                app.speech
                    .unregister_voice_owner(&binding.id, self.context.source_id())
                    .await;
                if let Err(error)=app.agents.control(self.context.principal(),&binding.id,"reload",json!({"overrides":{"systemPrompt":voice_prompt(binding.base_prompt.as_deref(),false)}})).await {tracing::warn!(%error,agent_id=%binding.id,"Failed to restore agent voice configuration");}
            }
        }
        Ok(())
    }
    /// Source createAbortController: abort in-flight speech and arm a fresh controller.
    async fn renew_output(&self) {
        let mut cancel = self.output_cancel.lock().await;
        cancel.cancel();
        *cancel = CancellationToken::new();
        self.playback.lock().await.clear();
    }
    /// Source handleAbort: abortController.abort() without re-arming, then
    /// cancelPendingPlaybacks. Speech stays suppressed until the next non-empty
    /// transcript re-arms the controller.
    async fn abort_output(&self) {
        self.output_cancel.lock().await.cancel();
        self.playback.lock().await.clear();
    }
    async fn abort(&self) -> Result<()> {
        self.abort_output().await;
        let id = self.binding.lock().await.as_ref().map(|b| b.id.clone());
        if let Some(id) = id {
            let app = self.app()?;
            if app.agents.has_in_flight_run(&id).await? {
                let result = app
                    .agents
                    .control(self.context.principal(), &id, "interrupt", json!({}))
                    .await;
                let result = result.and_then(|value| {
                    if value["status"] == "refused" {
                        Err(anyhow!("Agent {id} refused voice interruption"))
                    } else {
                        Ok(())
                    }
                });
                if let Err(error) = result {
                    self.emit(json!({"type":"activity_log","payload":{"id":uuid::Uuid::new_v4().to_string(),"timestamp":chrono::Utc::now().to_rfc3339(),"type":"error","content":format!("Voice interruption failed: {error}"),"metadata":{"voiceAbortFailed":true}}})).await?;
                    return Err(error);
                }
            }
        }
        Ok(())
    }
    async fn audio(self: &Arc<Self>, message: &Value) -> Result<()> {
        let bytes = STANDARD.decode(
            message["audio"]
                .as_str()
                .ok_or_else(|| anyhow!("Audio must be base64"))?,
        )?;
        let format = message["format"].as_str().unwrap_or("audio/wav");
        if bytes.is_empty() {
            return Ok(());
        }
        let active = self.binding.lock().await.is_some();
        let mut input = self.input.lock().await;
        if !active {
            if !input.buffered.is_empty()
                && input.buffered_format.to_ascii_lowercase().contains("pcm")
                    != format.to_ascii_lowercase().contains("pcm")
            {
                let old = std::mem::take(&mut input.buffered);
                let old_format = std::mem::take(&mut input.buffered_format);
                self.start_transcription(old, old_format, None).await;
            }
            if input.buffered.len() + bytes.len() > MAX_INPUT {
                bail!("Voice input buffer exceeds five minutes; finish or abort the recording")
            }
            input.buffered.extend_from_slice(&bytes);
            input.buffered_format = if format.to_ascii_lowercase().contains("pcm") {
                format!(
                    "pcm;rate={}",
                    audio::parse_pcm_rate_from_format(format, Some(16000)).unwrap_or(16000)
                )
            } else {
                format.to_owned()
            };
            // Match the legacy PCM chunk batching rule: once a second is buffered,
            // only an explicit final chunk closes that batch.
            if !message["isLast"].as_bool().unwrap_or(false)
                && format.to_ascii_lowercase().contains("pcm")
                && input.buffered.len() >= 32000
            {
                return Ok(());
            }
            let pcm = std::mem::take(&mut input.buffered);
            let format = std::mem::take(&mut input.buffered_format);
            drop(input);
            self.start_transcription(pcm, format, None).await;
            return Ok(());
        }
        let rate = audio::parse_pcm_rate_from_format(format, Some(16000)).unwrap_or(16000);
        if rate != input.rate {
            input.rate = rate;
            input.resampler = if rate == 16000 {
                None
            } else {
                Some(Pcm16MonoResampler::new(rate, 16000)?)
            };
        }
        let pcm = if let Some(resampler) = &mut input.resampler {
            resampler.process_chunk(&bytes)?
        } else {
            bytes
        };
        if input.pcm.len() + pcm.len() > MAX_INPUT {
            bail!("Voice input buffer exceeds five minutes without a completed turn")
        }
        input.pcm.extend_from_slice(&pcm);
        let vad = input.vad.clone().ok_or_else(|| {
            anyhow!("Voice mode is enabled but the voice turn controller is not running")
        })?;
        let detections = vad
            .append(
                audio::pcm16le_to_float32(&pcm, 1.0)?,
                self.owner.cancellation(),
            )
            .await?;
        let mut began = false;
        let mut stopped = false;
        for detected in detections {
            input.window_ms += 32;
            let now = input.window_ms;
            input.phase = match input.phase {
                Phase::Idle if detected => Phase::Confirming(now),
                Phase::Confirming(_) if !detected => Phase::Idle,
                Phase::Confirming(start) if now - start >= 800 => {
                    began = true;
                    input.started = Some(Instant::now());
                    Phase::Speaking
                }
                Phase::Speaking if !detected => Phase::Ending(now),
                Phase::Ending(_) if detected => Phase::Speaking,
                Phase::Ending(start) if now - start >= 1000 => {
                    stopped = true;
                    Phase::Idle
                }
                ref phase => match phase {
                    Phase::Idle => Phase::Idle,
                    Phase::Confirming(s) => Phase::Confirming(*s),
                    Phase::Speaking => Phase::Speaking,
                    Phase::Ending(s) => Phase::Ending(*s),
                },
            };
        }
        let completed = if stopped {
            Some((
                std::mem::take(&mut input.pcm),
                input.started.take().map(|s| s.elapsed().as_millis() as u64),
            ))
        } else {
            None
        };
        drop(input);
        if began {
            self.turn.fetch_add(1, Ordering::SeqCst);
            self.emit(json!({"type":"voice_input_state","payload":{"isSpeaking":true}}))
                .await?;
            self.abort().await?;
        }
        if let Some((pcm, duration)) = completed {
            vad.reset().await?;
            self.emit(json!({"type":"voice_input_state","payload":{"isSpeaking":false}}))
                .await?;
            self.start_transcription(pcm, "pcm;rate=16000".into(), duration)
                .await;
        }
        Ok(())
    }
    async fn start_transcription(
        self: &Arc<Self>,
        bytes: Vec<u8>,
        format: String,
        duration: Option<u64>,
    ) {
        let source = Arc::downgrade(self);
        let turn = self.turn.load(Ordering::SeqCst);
        let cancel = self.owner.cancellation();
        let task = tokio::spawn(async move {
            let Some(this) = source.upgrade() else { return };
            let _serial = tokio::select! { _=cancel.cancelled()=>return, guard=this.transcription_serial.lock()=>guard };
            let result=async {
                this.log("system","Transcribing audio...").await?;
                let app=this.app()?;
                let request_id=uuid::Uuid::new_v4().to_string();
                let (bytes,format)=if duration.is_none() && format.to_ascii_lowercase().contains("pcm") {
                    let rate=audio::parse_pcm_rate_from_format(&format,Some(16000)).unwrap_or(16000);
                    (Bytes::from(audio::pcm16_mono_wav(&bytes,rate)?),"audio/wav".to_owned())
                } else {(Bytes::from(bytes),format)};
                let environment=app.config.environment();
                let recording=if duration.is_none() {
                    match super::session_audio::persist(&environment,"STT_DEBUG_AUDIO_DIR",this.context.session_id(),&["buffered",&request_id],&format,&bytes).await {
                        Ok(path)=>path,Err(error)=>{tracing::warn!(%error,"Failed to persist input debug audio");None}
                    }
                }else{None};
                let result=if duration.is_none() {
                    super::session_audio::transcribe_batch(&app.speech,bytes,&format,&environment,&cancel).await?
                }else{
                    match tokio::time::timeout(std::time::Duration::from_secs(10),app.speech.transcribe(false,bytes,&format,&cancel)).await {
                        Ok(result)=>result?,
                        Err(_)=>{this.log("system","Timed out waiting for final voice transcript after 10000ms").await?;json!({"text":""})}
                    }
                };
                if this.turn.load(Ordering::SeqCst)!=turn || cancel.is_cancelled() {return Ok::<(),anyhow::Error>(())}
                let mut payload=result;
                if payload["isLowConfidence"]==true {payload["text"]=json!("");}
                if let Some(duration)=duration {payload["duration"]=json!(duration);}
                payload["requestId"]=json!(request_id);
                if let Some(path)=recording {payload["debugRecordingPath"]=json!(path);}
                let text=payload["text"].as_str().unwrap_or("").trim().to_owned();
                this.emit(json!({"type":"transcription_result","payload":payload})).await?;
                if let Some(path)=payload["debugRecordingPath"].as_str() {
                    this.emit(json!({"type":"activity_log","payload":{"id":uuid::Uuid::new_v4().to_string(),"timestamp":chrono::Utc::now().to_rfc3339(),"type":"system","content":format!("Saved input audio: {path}"),"metadata":{"recordingPath":path,"format":format,"requestId":request_id}}})).await?;
                }
                if text.is_empty() {return Ok(())}
                this.renew_output().await;
                this.log("transcript",&text).await?;
                let id=this.binding.lock().await.as_ref().map(|b|b.id.clone());
                if let Some(id)=id {
                    let spoken=format!("<spoken-input>\n{text}\n</spoken-input>\n<instruction>This message was spoken by the user. Respond using the speak tool only, not normal messages, because the user may not be looking at the chat.</instruction>");
                    app.agents.send(this.context.principal(),&id,json!({"text":spoken,"runOptions":{"spokenInput":true}})).await?;
                }
                Ok(())
            }.await;
            if let Err(error) = result {
                if !cancel.is_cancelled() {
                    let _ = this
                        .log("error", &format!("Transcription error: {error}"))
                        .await;
                }
            }
        });
        let mut tasks = self.transcriptions.lock().await;
        tasks.retain(|t| !t.is_finished());
        tasks.push(task);
    }
    /// Source voice speak handler around TTSManager.generateAndWaitForPlayback. Aborts
    /// (tool signal, voice abort, closed source) finish the call without an error.
    pub async fn speak(&self, text: &str, cancel: &CancellationToken) -> Result<()> {
        let segments = split_text(text)?;
        let app = self.app()?;
        let abort = SpeakAbort {
            tool: cancel.clone(),
            output: self.output_cancel.lock().await.clone(),
            source: self.owner.cancellation(),
        };
        let mut emitted_ids = Vec::new();
        let result = self
            .play_segments(&app, segments, &abort, &mut emitted_ids)
            .await;
        {
            let mut pending = self.playback.lock().await;
            for id in &emitted_ids {
                pending.remove(id);
            }
        }
        result?;
        // The source emitter drops messages once the voice source is closed.
        if abort.source.is_cancelled() {
            return Ok(());
        }
        self.log("assistant", text).await
    }
    async fn play_segments(
        &self,
        app: &Arc<AppState>,
        segments: Vec<String>,
        abort: &SpeakAbort,
        emitted_ids: &mut Vec<String>,
    ) -> Result<()> {
        // Prefetched synthesis runs as tasks so the next segments synthesize while
        // the client plays the current one.
        let synthesis = CancellationToken::new();
        let _cancel_synthesis = synthesis.clone().drop_guard();
        let mut pending = segments.into_iter();
        let mut inflight = PrefetchedSegments(VecDeque::new());
        inflight.schedule(&app.speech, &synthesis, &mut pending);
        while let Some(mut task) = inflight.0.pop_front() {
            if abort.is_cancelled() {
                task.abort();
                return Ok(());
            }
            let joined = tokio::select! {
                biased;
                _ = abort.cancelled() => {
                    task.abort();
                    return Ok(());
                }
                joined = &mut task => joined,
            };
            inflight.schedule(&app.speech, &synthesis, &mut pending);
            if abort.is_cancelled() {
                return Ok(());
            }
            let audio = joined.map_err(|error| anyhow!("TTS synthesis task failed: {error}"))??;
            if audio.bytes.is_empty() {
                continue;
            }
            let id = uuid::Uuid::new_v4().to_string();
            let (tx, rx) = oneshot::channel();
            self.playback.lock().await.insert(id.clone(), tx);
            emitted_ids.push(id.clone());
            self.emit(json!({"type":"audio_output","payload":{"id":format!("{id}:0"),"groupId":id,"chunkIndex":0,"isLastChunk":true,"audio":STANDARD.encode(&audio.bytes),"format":audio.format,"isVoiceMode":true}})).await?;
            match super::session_audio::persist(&app.config.environment(),"TTS_DEBUG_AUDIO_DIR",self.context.session_id(),&[&id],&audio.format,&audio.bytes).await {
                Ok(Some(path))=>self.emit(json!({"type":"activity_log","payload":{"id":uuid::Uuid::new_v4().to_string(),"timestamp":chrono::Utc::now().to_rfc3339(),"type":"system","content":format!("Saved TTS audio: {}",path.display()),"metadata":{"recordingPath":path,"format":audio.format,"groupId":id}}})).await?,
                Ok(None)=>{},
                Err(error)=>tracing::warn!(%error,"Failed to persist TTS debug audio"),
            }
            // A cleared playback map resolves like cancelPendingPlaybacks; the
            // accompanying abort is observed at the top of the loop.
            tokio::select! {
                biased;
                _ = abort.cancelled() => return Ok(()),
                _ = rx => {}
            }
            self.playback.lock().await.remove(&id);
        }
        Ok(())
    }
    pub async fn shutdown(&self) -> Result<()> {
        self.abort_output().await;
        self.dictation.shutdown().await?;
        self.turn.fetch_add(1, Ordering::SeqCst);
        let tasks = std::mem::take(&mut *self.transcriptions.lock().await);
        for task in tasks {
            task.abort();
            let _ = task.await;
        }
        let _serial = self.lifecycle.lock().await;
        self.disable().await?;
        self.owner.release().await
    }
}

/// Source AbortSignal.any([tool signal, voice abort controller]); closing the source
/// aborts the controller.
struct SpeakAbort {
    tool: CancellationToken,
    output: CancellationToken,
    source: CancellationToken,
}
impl SpeakAbort {
    fn is_cancelled(&self) -> bool {
        self.tool.is_cancelled() || self.output.is_cancelled() || self.source.is_cancelled()
    }
    async fn cancelled(&self) {
        tokio::select! {
            _ = self.tool.cancelled() => {}
            _ = self.output.cancelled() => {}
            _ = self.source.cancelled() => {}
        }
    }
}

/// Ordered in-flight synthesis, at most TTS_PREFETCH_SEGMENTS at a time.
struct PrefetchedSegments(VecDeque<tokio::task::JoinHandle<Result<super::cloud::SpeechAudio>>>);
impl PrefetchedSegments {
    fn schedule(
        &mut self,
        speech: &Arc<super::SpeechService>,
        cancel: &CancellationToken,
        pending: &mut impl Iterator<Item = String>,
    ) {
        while self.0.len() < TTS_PREFETCH_SEGMENTS {
            let Some(text) = pending.next() else { break };
            let speech = speech.clone();
            let cancel = cancel.clone();
            self.0
                .push_back(tokio::spawn(async move { speech.synthesize(&text, &cancel).await }));
        }
    }
}
impl Drop for PrefetchedSegments {
    fn drop(&mut self) {
        for task in &self.0 {
            task.abort();
        }
    }
}

fn strip_voice_prompt(existing: Option<&str>) -> Option<String> {
    let mut text = existing?.trim().to_owned();
    while let Some(start) = text.find("<paseo_voice_mode>") {
        let Some(end) = text[start..].find("</paseo_voice_mode>") else {
            break;
        };
        text.replace_range(start..start + end + "</paseo_voice_mode>".len(), "");
    }
    let text = text.trim();
    if text.is_empty() {
        None
    } else {
        Some(text.to_owned())
    }
}
fn voice_prompt(base: Option<&str>, enabled: bool) -> String {
    let instruction = if enabled {
        "Paseo voice mode is now on. You are the Paseo voice assistant. The user cannot see your chat messages or tool calls. Always use the speak tool for all user-facing communication. Before calling any non-speak tool, first call speak with a short acknowledgement of what you heard and what you will do next. For long-running work, use speak to provide progress updates before and during execution. Treat the user input as transcribed speech. If the user intent is clear, proceed without extra confirmation. If the transcription seems incomplete, cut off, ambiguous, or may contain a non-obvious mistake or misspelling, ask a clarifying question via speak before taking action. Use concise plain language suitable for speech output."
    } else {
        "Paseo voice mode is now off. Ignore any earlier Paseo voice mode instructions in this thread."
    };
    let block = format!("<paseo_voice_mode>\n{instruction}\n</paseo_voice_mode>");
    match base {
        Some(base) if !base.is_empty() => format!("{base}\n\n{block}"),
        _ => block,
    }
}
fn split_text(text: &str) -> Result<Vec<String>> {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() {
        bail!("Cannot synthesize empty text")
    }
    let mut sentences = Vec::new();
    let mut start = 0;
    for (index, ch) in text.char_indices() {
        if ch == ' '
            && text[..index]
                .chars()
                .next_back()
                .is_some_and(|c| ".!?".contains(c))
        {
            sentences.push(&text[start..index]);
            start = index + 1;
        }
    }
    sentences.push(&text[start..]);
    let mut result = Vec::new();
    for sentence in sentences {
        split_fragment(sentence, &mut result);
    }
    Ok(result)
}
fn split_fragment(fragment: &str, out: &mut Vec<String>) {
    let fragment = fragment.trim();
    if fragment.is_empty() {
        return;
    }
    if fragment.encode_utf16().count() <= MAX_TTS_SEGMENT_CHARS {
        out.push(fragment.to_owned());
        return;
    }
    let mut clauses = Vec::new();
    let mut start = 0;
    for (index, ch) in fragment.char_indices() {
        if ch == ' '
            && fragment[..index]
                .chars()
                .next_back()
                .is_some_and(|c| ",;:".contains(c))
        {
            clauses.push(&fragment[start..index]);
            start = index + 1;
        }
    }
    if !clauses.is_empty() {
        clauses.push(&fragment[start..]);
        let mut current = String::new();
        for clause in clauses {
            if clause.encode_utf16().count() > MAX_TTS_SEGMENT_CHARS {
                if !current.is_empty() {
                    out.push(std::mem::take(&mut current));
                }
                split_fragment(clause, out);
            } else if current.is_empty() {
                current = clause.to_owned();
            } else if current.encode_utf16().count() + 1 + clause.encode_utf16().count()
                <= MAX_TTS_SEGMENT_CHARS
            {
                current.push(' ');
                current.push_str(clause);
            } else {
                out.push(std::mem::replace(&mut current, clause.to_owned()));
            }
        }
        if !current.is_empty() {
            out.push(current);
        }
        return;
    }
    // Source: lastIndexOf(" ", 260), cutting at 260 units when that space precedes 130.
    let mut remaining = fragment;
    while remaining.encode_utf16().count() > MAX_TTS_SEGMENT_CHARS {
        let mut position = 0;
        let mut cut = 0;
        let mut space = None;
        for (index, ch) in remaining.char_indices() {
            if position > MAX_TTS_SEGMENT_CHARS {
                break;
            }
            if ch == ' ' && position >= MAX_TTS_SEGMENT_CHARS / 2 {
                space = Some(index);
            }
            position += ch.len_utf16();
            if position <= MAX_TTS_SEGMENT_CHARS {
                cut = index + ch.len_utf8();
            }
        }
        let cut = space.unwrap_or(cut);
        out.push(remaining[..cut].trim().to_owned());
        remaining = remaining[cut..].trim();
    }
    if !remaining.is_empty() {
        out.push(remaining.to_owned());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn voice_prompt_replacement_preserves_base() {
        assert_eq!(
            strip_voice_prompt(Some(" base <paseo_voice_mode>old</paseo_voice_mode> ")).as_deref(),
            Some("base")
        );
        assert!(!voice_prompt(Some("base"), false).contains("now on"));
    }
    #[test]
    fn tts_segments_preserve_sentences_and_unicode() {
        let text = format!("First. {}", "안녕 ".repeat(180));
        let segments = split_text(&text).unwrap();
        assert_eq!(segments[0], "First.");
        assert!(segments.iter().all(|s| s.encode_utf16().count() <= 260));
        assert_eq!(
            segments.join(" "),
            text.split_whitespace().collect::<Vec<_>>().join(" ")
        );
    }
    #[test]
    fn tts_hard_split_ignores_spaces_before_half_the_segment_limit() {
        // lastIndexOf(" ", 260) finds index 129 < 130, so the source cuts at 260 units.
        let text = format!("{} {}", "a".repeat(129), "b".repeat(200));
        assert_eq!(
            split_text(&text).unwrap(),
            vec![
                format!("{} {}", "a".repeat(129), "b".repeat(130)),
                "b".repeat(70)
            ]
        );
        let text = format!("{} {}", "a".repeat(130), "b".repeat(200));
        assert_eq!(
            split_text(&text).unwrap(),
            vec!["a".repeat(130), "b".repeat(200)]
        );
    }
}
