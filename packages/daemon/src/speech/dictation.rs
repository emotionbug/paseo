use super::audio::{Pcm16MonoResampler, parse_pcm_rate_from_format, pcm16le_peak_abs};
use crate::{state::AppState, transport::OwnedOperation};
use anyhow::{Context, Result, anyhow};
use bytes::{Bytes, BytesMut};
use chrono::{SecondsFormat, Utc};
use futures_util::{FutureExt, StreamExt, future::BoxFuture, stream::FuturesUnordered};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap},
    path::{Component, Path, PathBuf},
    sync::{Arc, Weak},
    time::Duration,
};
use tokio::{
    io::AsyncWriteExt,
    sync::{Mutex, mpsc, oneshot, watch},
    time::Instant,
};
use tokio_util::sync::CancellationToken;

const FINAL_TIMEOUT_MS: u64 = 10_000;
const FINAL_TIMEOUT_MAX_MS: u64 = 300_000;
const PARTIAL_INTERVAL: Duration = Duration::from_millis(350);

/// One manager belongs to one physical transport source, never a reconnecting session.
pub struct DictationManager {
    inner: Arc<ManagerInner>,
}

struct ManagerInner {
    app: Weak<AppState>,
    owner: OwnedOperation,
    session_id: String,
    cancel: CancellationToken,
    streams: Mutex<HashMap<String, Arc<StreamHandle>>>,
}

struct StreamHandle {
    cancel: CancellationToken,
    commands: mpsc::Sender<Command>,
    done: watch::Receiver<bool>,
}

struct Command {
    kind: CommandKind,
    reply: oneshot::Sender<Result<()>>,
}

enum CommandKind {
    Chunk {
        seq: i64,
        audio: String,
        format: String,
    },
    Finish {
        final_seq: i64,
    },
}

impl DictationManager {
    pub fn new(app: Weak<AppState>, owner: OwnedOperation, session_id: String) -> Self {
        let cancel = owner.cancellation().child_token();
        Self {
            inner: Arc::new(ManagerInner {
                app,
                owner,
                session_id,
                cancel,
                streams: Mutex::new(HashMap::new()),
            }),
        }
    }

    pub async fn has_demand(&self) -> bool {
        !self.inner.cancel.is_cancelled() && !self.inner.streams.lock().await.is_empty()
    }

    pub async fn handle(&self, message: &Value) -> Result<bool> {
        let Some(kind) = message.get("type").and_then(Value::as_str) else {
            return Ok(false);
        };
        if !matches!(
            kind,
            "dictation_stream_start"
                | "dictation_stream_chunk"
                | "dictation_stream_finish"
                | "dictation_stream_cancel"
        ) {
            return Ok(false);
        }
        let id = string_field(message, "dictationId")?;
        if self.inner.cancel.is_cancelled() {
            return Ok(true);
        }
        match kind {
            "dictation_stream_start" => self.start(id, string_field(message, "format")?).await?,
            "dictation_stream_cancel" => {
                if let Some(stream) = self.inner.streams.lock().await.remove(id) {
                    stream.cancel.cancel();
                }
            }
            _ => {
                let stream = self.inner.streams.lock().await.get(id).cloned();
                let Some(stream) = stream else {
                    self.inner
                        .emit_error(id, "Dictation stream not started", true)
                        .await?;
                    return Ok(true);
                };
                let kind = if kind == "dictation_stream_chunk" {
                    CommandKind::Chunk {
                        seq: integer_field(message, "seq")?,
                        audio: string_field(message, "audio")?.to_owned(),
                        format: string_field(message, "format")?.to_owned(),
                    }
                } else {
                    CommandKind::Finish {
                        final_seq: integer_field(message, "finalSeq")?,
                    }
                };
                let (reply, response) = oneshot::channel();
                tokio::select! {
                    biased;
                    _ = stream.cancel.cancelled() => {},
                    sent = stream.commands.send(Command { kind, reply }) => {
                        if sent.is_ok() {
                            tokio::select! {
                                biased;
                                _ = stream.cancel.cancelled() => {},
                                result = response => { if let Ok(result) = result { result?; } },
                            }
                        }
                    }
                }
            }
        }
        Ok(true)
    }

    async fn start(&self, id: &str, format: &str) -> Result<()> {
        let (commands, receiver) = mpsc::channel(32);
        let (finished, done) = watch::channel(false);
        let handle = Arc::new(StreamHandle {
            cancel: self.inner.cancel.child_token(),
            commands,
            done,
        });
        {
            let mut streams = self.inner.streams.lock().await;
            if self.inner.cancel.is_cancelled() {
                return Ok(());
            }
            if let Some(previous) = streams.insert(id.to_owned(), handle.clone()) {
                previous.cancel.cancel();
            }
        }
        let (ready, response) = oneshot::channel();
        let inner = self.inner.clone();
        let task_handle = handle.clone();
        let id = id.to_owned();
        let format = format.to_owned();
        tokio::spawn(async move {
            let result = tokio::select! {
                biased;
                _ = task_handle.cancel.cancelled() => Ok(()),
                result = run_stream(inner.clone(), task_handle.cancel.clone(), &id, format, receiver, ready) => result,
            };
            if let Err(error) = result {
                tracing::warn!(dictation_id = %id, %error, "Dictation stream delivery failed");
            }
            task_handle.cancel.cancel();
            let mut streams = inner.streams.lock().await;
            if streams
                .get(&id)
                .is_some_and(|current| Arc::ptr_eq(current, &task_handle))
            {
                streams.remove(&id);
            }
            drop(streams);
            let _ = finished.send(true);
        });
        tokio::select! {
            biased;
            _ = handle.cancel.cancelled() => Ok(()),
            result = response => result.unwrap_or(Ok(())),
        }
    }

    pub async fn shutdown(&self) -> Result<()> {
        self.inner.cancel.cancel();
        let streams = std::mem::take(&mut *self.inner.streams.lock().await);
        for stream in streams.into_values() {
            stream.cancel.cancel();
            let mut done = stream.done.clone();
            let _ = done.wait_for(|finished| *finished).await;
        }
        Ok(())
    }
}

impl Drop for DictationManager {
    fn drop(&mut self) {
        self.inner.cancel.cancel();
    }
}

impl ManagerInner {
    async fn emit_error(&self, id: &str, error: &str, retryable: bool) -> Result<()> {
        self.owner
            .emit(json!({"type":"dictation_stream_error", "payload":{
                "dictationId":id, "error":error, "retryable":retryable,
            }}))
            .await
    }
}

async fn run_stream(
    manager: Arc<ManagerInner>,
    cancel: CancellationToken,
    id: &str,
    format: String,
    mut commands: mpsc::Receiver<Command>,
    ready: oneshot::Sender<Result<()>>,
) -> Result<()> {
    let initialized = async {
        let (speech, environment) = {
            let app = manager
                .app
                .upgrade()
                .ok_or_else(|| anyhow!("Speech service is shutting down"))?;
            (app.speech.clone(), app.config.environment())
        };
        let output_rate = speech.required_sample_rate(true).await?;
        let local = speech.is_local_stt(true).await;
        StreamState::new(
            id.to_owned(),
            format,
            output_rate,
            local,
            cancel,
            &environment,
            &manager.session_id,
        )
    }
    .await;
    let mut state = match initialized {
        Ok(state) => state,
        Err(error) => {
            let result = manager.emit_error(id, &error.to_string(), false).await;
            let _ = ready.send(result);
            return Ok(());
        }
    };
    let ack = state.ack(&manager).await;
    let ack_failed = ack.is_err();
    let _ = ready.send(ack);
    if ack_failed {
        return Ok(());
    }
    loop {
        enum Event {
            Command(Option<Command>),
            Decode(Option<DecodeResult>),
            Timeout,
        }
        let deadline = state.deadline.unwrap_or_else(Instant::now);
        let event = tokio::select! {
            command = commands.recv() => Event::Command(command),
            result = state.decodes.next(), if !state.decodes.is_empty() => Event::Decode(result),
            _ = tokio::time::sleep_until(deadline), if state.deadline.is_some() => Event::Timeout,
        };
        match event {
            Event::Command(None) => return Ok(()),
            Event::Timeout => {
                return state
                    .fail(&manager, "Timed out waiting for final transcription", true)
                    .await;
            }
            Event::Command(Some(command)) => {
                let result = match command.kind {
                    CommandKind::Chunk { seq, audio, format } => {
                        state.chunk(&manager, seq, audio, &format).await
                    }
                    CommandKind::Finish { final_seq } => state.finish(&manager, final_seq).await,
                };
                match result {
                    Ok(finished) => {
                        let _ = command.reply.send(Ok(()));
                        if finished {
                            return Ok(());
                        }
                    }
                    Err(error) => {
                        let result = state.fail(&manager, &error.to_string(), true).await;
                        let _ = command.reply.send(result);
                        return Ok(());
                    }
                }
            }
            Event::Decode(Some(result)) => match state.decoded(&manager, result).await {
                Ok(true) => return Ok(()),
                Ok(false) => {}
                Err(error) => return state.fail(&manager, &error.to_string(), true).await,
            },
            Event::Decode(None) => {}
        }
    }
}

struct Segment {
    id: u64,
    text: String,
    final_received: bool,
    from_preview: bool,
}

struct Preview {
    id: u64,
    bytes: usize,
    cancel: CancellationToken,
}

struct DecodeResult {
    id: u64,
    final_decode: bool,
    text: Result<String>,
}

struct StreamState {
    id: String,
    input_format: String,
    output_rate: u32,
    resampler: Option<Pcm16MonoResampler>,
    received: BTreeMap<i64, Bytes>,
    next_seq: i64,
    ack_seq: i64,
    auto_commit_bytes: usize,
    silence_threshold: f64,
    pcm: BytesMut,
    peak: u32,
    segments: Vec<Segment>,
    current_segment: u64,
    current_partial: String,
    local: bool,
    preview: Option<Preview>,
    last_decode_at: Option<Instant>,
    pending_decode: bool,
    decodes: FuturesUnordered<BoxFuture<'static, DecodeResult>>,
    cancel: CancellationToken,
    finish_requested: bool,
    finish_sealed: bool,
    final_seq: Option<i64>,
    deadline: Option<Instant>,
    debug: Option<DebugAudio>,
}

impl StreamState {
    fn new(
        id: String,
        input_format: String,
        output_rate: u32,
        local: bool,
        cancel: CancellationToken,
        environment: &BTreeMap<String, String>,
        session_id: &str,
    ) -> Result<Self> {
        let input_rate = parse_pcm_rate_from_format(&input_format, Some(16_000)).unwrap_or(16_000);
        let resampler = if input_rate == output_rate {
            None
        } else {
            Some(Pcm16MonoResampler::new(input_rate, output_rate)?)
        };
        let seconds = environment
            .get("PASEO_DICTATION_AUTO_COMMIT_SECONDS")
            .and_then(|value| parse_float_prefix(value))
            .filter(|value| value.is_finite() && *value >= 0.0)
            .unwrap_or(15.0);
        let auto_commit_bytes = if seconds > 0.0 {
            ((seconds * f64::from(output_rate) * 2.0 + 0.5).floor() as usize).max(1)
        } else {
            0
        };
        // A malformed parseInt in the source is NaN, which disables the comparison.
        let silence_threshold = environment
            .get("PASEO_DICTATION_SILENCE_PEAK_THRESHOLD")
            .map(|value| parse_integer_prefix(value).unwrap_or(f64::NAN))
            .unwrap_or(300.0);
        let debug = DebugAudio::new(environment, session_id, &id)?;
        Ok(Self {
            id,
            input_format,
            output_rate,
            resampler,
            received: BTreeMap::new(),
            next_seq: 0,
            ack_seq: -1,
            auto_commit_bytes,
            silence_threshold,
            pcm: BytesMut::new(),
            peak: 0,
            segments: Vec::new(),
            current_segment: 0,
            current_partial: String::new(),
            local,
            preview: None,
            last_decode_at: None,
            pending_decode: false,
            decodes: FuturesUnordered::new(),
            cancel,
            finish_requested: false,
            finish_sealed: false,
            final_seq: None,
            deadline: None,
            debug,
        })
    }

    async fn ack(&mut self, manager: &ManagerInner) -> Result<()> {
        manager
            .owner
            .emit(json!({"type":"dictation_stream_ack", "payload":{
                "dictationId":self.id, "ackSeq":self.ack_seq,
            }}))
            .await
    }

    async fn chunk(
        &mut self,
        manager: &ManagerInner,
        seq: i64,
        audio: String,
        format: &str,
    ) -> Result<bool> {
        if format != self.input_format {
            self.fail(
                manager,
                &format!("Mismatched dictation stream format: {format}"),
                false,
            )
            .await?;
            return Ok(true);
        }
        if seq < self.next_seq {
            self.ack(manager).await?;
            return Ok(false);
        }
        self.received
            .entry(seq)
            .or_insert_with(|| decode_base64(&audio));
        while let Some(pcm) = self.received.remove(&self.next_seq) {
            let pcm = if let Some(resampler) = &mut self.resampler {
                Bytes::from(resampler.process_chunk(&pcm)?)
            } else {
                pcm
            };
            if !pcm.is_empty() {
                self.peak = self.peak.max(pcm16le_peak_abs(&pcm)?);
                self.pcm.extend_from_slice(&pcm);
                if let Some(debug) = &mut self.debug {
                    debug.chunk(self.next_seq, pcm).await;
                }
                if !self.finish_requested
                    && self.auto_commit_bytes > 0
                    && self.pcm.len() >= self.auto_commit_bytes
                {
                    if self.is_silence() {
                        self.clear_current();
                    } else {
                        self.commit(manager);
                    }
                } else {
                    self.maybe_preview(manager, false);
                }
            }
            self.next_seq += 1;
            self.ack_seq = self.next_seq - 1;
        }
        self.ack(manager).await?;
        self.seal_finish(manager);
        self.maybe_finalize(manager).await
    }

    async fn finish(&mut self, manager: &ManagerInner, final_seq: i64) -> Result<bool> {
        self.finish_requested = true;
        self.final_seq = Some(final_seq);
        if final_seq >= 0 && self.ack_seq < 0 && self.next_seq == 0 && self.received.is_empty() {
            manager.emit_error(&self.id, &format!("Dictation finished (finalSeq={final_seq}) but no audio chunks were received"), true).await?;
            return Ok(true);
        }
        self.seal_finish(manager);
        let timeout_ms = self.timeout_ms();
        self.deadline = Some(Instant::now() + Duration::from_millis(timeout_ms));
        manager
            .owner
            .emit(
                json!({"type":"dictation_stream_finish_accepted", "payload":{
                    "dictationId":self.id, "timeoutMs":timeout_ms,
                }}),
            )
            .await?;
        self.maybe_finalize(manager).await
    }

    fn is_silence(&self) -> bool {
        f64::from(self.peak) < self.silence_threshold
    }

    fn clear_current(&mut self) {
        if let Some(preview) = self.preview.take() {
            preview.cancel.cancel();
        }
        self.current_segment += 1;
        self.current_partial.clear();
        self.pcm.clear();
        self.peak = 0;
        self.pending_decode = false;
    }

    fn commit(&mut self, manager: &ManagerInner) {
        let id = self.current_segment;
        let audio = self.pcm.split().freeze();
        let reuse_preview = self
            .preview
            .as_ref()
            .is_some_and(|preview| preview.id == id && preview.bytes == audio.len());
        self.segments.push(Segment {
            id,
            text: std::mem::take(&mut self.current_partial),
            final_received: false,
            from_preview: reuse_preview,
        });
        if !reuse_preview {
            if let Some(preview) = self.preview.take() {
                preview.cancel.cancel();
            }
            self.decode(manager, id, true, audio, self.cancel.child_token());
        } else {
            self.preview = None;
        }
        self.current_segment += 1;
        self.peak = 0;
        self.pending_decode = false;
    }

    fn seal_finish(&mut self, manager: &ManagerInner) {
        if !self.finish_requested
            || self.finish_sealed
            || self.final_seq.is_none_or(|seq| self.ack_seq < seq)
        {
            return;
        }
        if !self.pcm.is_empty() {
            if self.is_silence() {
                self.clear_current();
            } else {
                self.commit(manager);
            }
        }
        self.finish_sealed = true;
    }

    fn maybe_preview(&mut self, manager: &ManagerInner, force: bool) {
        if !self.local || self.pcm.is_empty() || self.finish_sealed {
            return;
        }
        if !force
            && self
                .last_decode_at
                .is_some_and(|last| last.elapsed() < PARTIAL_INTERVAL)
        {
            return;
        }
        if self.preview.is_some() {
            self.pending_decode = true;
            return;
        }
        let cancel = self.cancel.child_token();
        let id = self.current_segment;
        let bytes = self.pcm.len();
        // A preview must own a stable snapshot while later chunks extend the segment.
        self.decode(
            manager,
            id,
            false,
            Bytes::copy_from_slice(&self.pcm),
            cancel.clone(),
        );
        self.preview = Some(Preview { id, bytes, cancel });
    }

    fn decode(
        &mut self,
        manager: &ManagerInner,
        id: u64,
        final_decode: bool,
        audio: Bytes,
        cancel: CancellationToken,
    ) {
        let app = manager.app.clone();
        let rate = self.output_rate;
        self.decodes.push(
            async move {
                let result = async {
                    let speech = app
                        .upgrade()
                        .ok_or_else(|| anyhow!("Speech service is shutting down"))?
                        .speech
                        .clone();
                    let result = speech
                        .transcribe(true, audio, &format!("pcm;rate={rate}"), &cancel)
                        .await?;
                    // Dictation, unlike voice turn admission, keeps low-confidence text.
                    result
                        .get("text")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                        .ok_or_else(|| anyhow!("Transcription result is missing text"))
                };
                let text = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => Err(anyhow!("Dictation transcription cancelled")),
                    result = result => result,
                };
                DecodeResult {
                    id,
                    final_decode,
                    text,
                }
            }
            .boxed(),
        );
    }

    async fn decoded(&mut self, manager: &ManagerInner, result: DecodeResult) -> Result<bool> {
        if let Some(index) = self
            .segments
            .iter()
            .position(|segment| segment.id == result.id)
        {
            if !result.final_decode && !self.segments[index].from_preview {
                return Ok(false);
            }
            let text = match result.text {
                Ok(text) => text,
                Err(error) if self.finish_requested && is_buffer_too_small(&error.to_string()) => {
                    self.segments.remove(index);
                    return self.maybe_finalize(manager).await;
                }
                Err(error) => return Err(error),
            };
            self.segments[index].text = text;
            self.segments[index].final_received = true;
            if self.local {
                self.last_decode_at = Some(Instant::now());
            }
            self.emit_partial(manager, false).await?;
        } else if !result.final_decode && result.id == self.current_segment {
            self.preview = None;
            let text = match result.text {
                Ok(text) => text,
                Err(error) if self.finish_requested && is_buffer_too_small(&error.to_string()) => {
                    return self.maybe_finalize(manager).await;
                }
                Err(error) => return Err(error),
            };
            self.last_decode_at = Some(Instant::now());
            if text != self.current_partial {
                self.current_partial = text;
                self.emit_partial(manager, true).await?;
            }
            if self.pending_decode {
                self.pending_decode = false;
                self.maybe_preview(manager, true);
            }
        } else {
            return Ok(false);
        }
        self.seal_finish(manager);
        self.maybe_finalize(manager).await
    }

    async fn emit_partial(&mut self, manager: &ManagerInner, include_current: bool) -> Result<()> {
        let current = include_current.then_some(self.current_partial.as_str());
        let text = ordered_text(
            self.segments
                .iter()
                .map(|segment| segment.text.as_str())
                .chain(current),
        );
        manager
            .owner
            .emit(json!({"type":"dictation_stream_partial", "payload":{
                "dictationId":self.id, "text":text,
            }}))
            .await
    }

    fn timeout_ms(&self) -> u64 {
        final_timeout_ms(
            self.segments
                .iter()
                .filter(|segment| !segment.final_received)
                .count(),
            self.pcm.len(),
            self.output_rate,
            self.final_seq
                .map_or(0, |seq| seq.saturating_sub(self.ack_seq).max(0) as u64),
        )
    }

    async fn maybe_finalize(&mut self, manager: &ManagerInner) -> Result<bool> {
        if !self.finish_requested
            || !self.finish_sealed
            || self.final_seq.is_none_or(|seq| self.ack_seq < seq)
            || self.segments.iter().any(|segment| !segment.final_received)
        {
            return Ok(false);
        }
        let text = ordered_text(self.segments.iter().map(|segment| segment.text.as_str()));
        self.emit_terminal(
            manager,
            "dictation_stream_final",
            json!({"dictationId":self.id, "text":text}),
        )
        .await?;
        Ok(true)
    }

    async fn fail(&mut self, manager: &ManagerInner, error: &str, retryable: bool) -> Result<()> {
        self.emit_terminal(
            manager,
            "dictation_stream_error",
            json!({
                "dictationId":self.id, "error":error, "retryable":retryable,
            }),
        )
        .await
    }

    async fn emit_terminal(
        &mut self,
        manager: &ManagerInner,
        kind: &str,
        mut payload: Value,
    ) -> Result<()> {
        let recording = if let Some(debug) = &mut self.debug {
            debug.persist(self.output_rate).await?
        } else {
            None
        };
        if let Some(path) = &recording {
            payload["debugRecordingPath"] = json!(path);
        }
        manager
            .owner
            .emit(json!({"type":kind, "payload":payload}))
            .await?;
        if let Some(path) = recording {
            manager
                .owner
                .emit(json!({"type":"activity_log", "payload":{
                    "id":uuid::Uuid::new_v4().to_string(),
                    "timestamp":Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
                    "type":"system", "content":format!("Saved dictation audio: {path}"),
                    "metadata":{"recordingPath":path, "dictationId":self.id},
                }}))
                .await?;
        }
        Ok(())
    }
}

fn final_timeout_ms(
    pending_segments: usize,
    pending_bytes: usize,
    rate: u32,
    missing_seq: u64,
) -> u64 {
    let bytes_per_second = u64::from(rate).saturating_mul(2).max(1);
    let seconds = (pending_bytes as u64).div_ceil(bytes_per_second);
    FINAL_TIMEOUT_MS
        .saturating_add((pending_segments as u64).saturating_mul(15_000))
        .saturating_add(seconds.saturating_mul(1_500))
        .saturating_add(missing_seq.saturating_mul(250))
        .min(FINAL_TIMEOUT_MAX_MS)
}

fn ordered_text<'a>(parts: impl Iterator<Item = &'a str>) -> String {
    let mut text = String::new();
    for (index, part) in parts.enumerate() {
        if index > 0 {
            text.push(' ');
        }
        text.push_str(part);
    }
    let trimmed = text.trim_matches(is_js_whitespace);
    if trimmed.len() == text.len() {
        return text;
    }
    let start = text.len() - text.trim_start_matches(is_js_whitespace).len();
    let end = start + trimmed.len();
    text.truncate(end);
    if start > 0 {
        text.drain(..start);
    }
    text
}

fn is_js_whitespace(character: char) -> bool {
    matches!(character, '\u{0009}'..='\u{000d}' | '\u{0020}' | '\u{00a0}' | '\u{1680}'
        | '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}'
        | '\u{3000}' | '\u{feff}')
}

fn is_buffer_too_small(error: &str) -> bool {
    error
        .as_bytes()
        .windows(b"buffer too small".len())
        .any(|window| window.eq_ignore_ascii_case(b"buffer too small"))
}

fn string_field<'a>(message: &'a Value, key: &str) -> Result<&'a str> {
    message
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("Dictation {key} is required"))
}

fn integer_field(message: &Value, key: &str) -> Result<i64> {
    message
        .get(key)
        .and_then(Value::as_i64)
        .ok_or_else(|| anyhow!("Dictation {key} must be an integer"))
}

// Node Buffer.from(value, "base64") accepts URL-safe alphabet, unpadded input,
// and ignorable non-alphabet characters, and stops at the first padding byte.
fn decode_base64(value: &str) -> Bytes {
    let mut output = Vec::with_capacity(value.len().saturating_mul(3) / 4);
    let mut bits = 0u32;
    let mut pending = 0u32;
    for byte in value.bytes() {
        let digit = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            _ => continue,
        };
        pending = (pending << 6) | u32::from(digit);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            output.push((pending >> bits) as u8);
            pending &= (1 << bits) - 1;
        }
    }
    output.into()
}

fn parse_float_prefix(value: &str) -> Option<f64> {
    let value = value.trim_start();
    let bytes = value.as_bytes();
    let mut end = usize::from(matches!(bytes.first(), Some(b'+' | b'-')));
    let mut digits = 0;
    while bytes.get(end).is_some_and(u8::is_ascii_digit) {
        end += 1;
        digits += 1;
    }
    if bytes.get(end) == Some(&b'.') {
        end += 1;
        while bytes.get(end).is_some_and(u8::is_ascii_digit) {
            end += 1;
            digits += 1;
        }
    }
    if digits == 0 {
        return None;
    }
    if matches!(bytes.get(end), Some(b'e' | b'E')) {
        let mut exponent = end + 1;
        if matches!(bytes.get(exponent), Some(b'+' | b'-')) {
            exponent += 1;
        }
        let start = exponent;
        while bytes.get(exponent).is_some_and(u8::is_ascii_digit) {
            exponent += 1;
        }
        if exponent > start {
            end = exponent;
        }
    }
    value[..end].parse().ok()
}

fn parse_integer_prefix(value: &str) -> Option<f64> {
    let value = value.trim_start();
    let bytes = value.as_bytes();
    let start = usize::from(matches!(bytes.first(), Some(b'+' | b'-')));
    let mut end = start;
    while bytes.get(end).is_some_and(u8::is_ascii_digit) {
        end += 1;
    }
    if end == start {
        None
    } else {
        value[..end].parse().ok()
    }
}

struct DebugAudio {
    folder: PathBuf,
    combined_enabled: bool,
    chunks: Vec<Bytes>,
    recording: Option<String>,
}

impl DebugAudio {
    fn new(
        environment: &BTreeMap<String, String>,
        session_id: &str,
        dictation_id: &str,
    ) -> Result<Option<Self>> {
        let enabled = environment
            .get("PASEO_DICTATION_DEBUG")
            .is_some_and(|value| {
                matches!(
                    value.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes" | "on"
                )
            });
        let explicit = environment
            .get("DICTATION_DEBUG_AUDIO_DIR")
            .map(|value| value.trim())
            .filter(|value| !value.is_empty());
        if !enabled && explicit.is_none() {
            return Ok(None);
        }
        let root = Path::new(explicit.unwrap_or(".debug/recordings"));
        let root = if root.is_absolute() {
            root.to_owned()
        } else {
            std::env::current_dir()?.join(root)
        };
        let root = normalize_path(&root);
        let timestamp = Utc::now()
            .to_rfc3339_opts(SecondsFormat::Millis, true)
            .replace([':', '.'], "-");
        let folder = root
            .join(sanitize_filename(session_id, "session"))
            .join(format!(
                "{timestamp}_{}",
                sanitize_filename(dictation_id, "dictation")
            ));
        Ok(Some(Self {
            folder,
            combined_enabled: enabled,
            chunks: Vec::new(),
            recording: None,
        }))
    }

    async fn chunk(&mut self, seq: i64, audio: Bytes) {
        let write = async {
            tokio::fs::create_dir_all(&self.folder).await?;
            tokio::fs::write(self.folder.join(format!("chunk_{seq:06}.pcm")), &audio).await
        }
        .await;
        if let Err(error) = write {
            tracing::warn!(%error, "Failed to write dictation debug chunk");
        }
        if self.combined_enabled {
            self.chunks.push(audio);
        }
    }

    async fn persist(&mut self, sample_rate: u32) -> Result<Option<String>> {
        if !self.combined_enabled || self.chunks.is_empty() {
            return Ok(None);
        }
        if let Some(path) = &self.recording {
            return Ok(Some(path.clone()));
        }
        let size = self
            .chunks
            .iter()
            .try_fold(0u32, |size, chunk| {
                size.checked_add(u32::try_from(chunk.len()).ok()?)
                    .filter(|size| *size <= u32::MAX - 36)
            })
            .ok_or_else(|| anyhow!("Dictation WAV is too large"))?;
        let byte_rate = sample_rate
            .checked_mul(2)
            .ok_or_else(|| anyhow!("Dictation WAV sample rate is too large"))?;
        let mut header = [0u8; 44];
        header[0..4].copy_from_slice(b"RIFF");
        header[4..8].copy_from_slice(&(size + 36).to_le_bytes());
        header[8..16].copy_from_slice(b"WAVEfmt ");
        header[16..20].copy_from_slice(&16u32.to_le_bytes());
        header[20..22].copy_from_slice(&1u16.to_le_bytes());
        header[22..24].copy_from_slice(&1u16.to_le_bytes());
        header[24..28].copy_from_slice(&sample_rate.to_le_bytes());
        header[28..32].copy_from_slice(&byte_rate.to_le_bytes());
        header[32..34].copy_from_slice(&2u16.to_le_bytes());
        header[34..36].copy_from_slice(&16u16.to_le_bytes());
        header[36..40].copy_from_slice(b"data");
        header[40..44].copy_from_slice(&size.to_le_bytes());
        tokio::fs::create_dir_all(&self.folder).await?;
        let path = self.folder.join("combined.wav");
        let mut file = tokio::fs::File::create(&path)
            .await
            .context("Creating dictation debug recording")?;
        file.write_all(&header).await?;
        for chunk in &self.chunks {
            file.write_all(chunk).await?;
        }
        file.flush().await?;
        let path = path.to_string_lossy().into_owned();
        self.recording = Some(path.clone());
        Ok(Some(path))
    }
}

fn sanitize_filename(value: &str, fallback: &str) -> String {
    let value = if value.is_empty() { fallback } else { value };
    value
        .encode_utf16()
        .take(64)
        .map(|unit| {
            if unit <= 127
                && ((unit as u8).is_ascii_alphanumeric()
                    || unit == b'-' as u16
                    || unit == b'_' as u16)
            {
                char::from(unit as u8)
            } else {
                '_'
            }
        })
        .collect()
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeout_tracks_pending_segments_audio_and_missing_sequences() {
        assert_eq!(final_timeout_ms(0, 0, 24_000, 0), 10_000);
        assert_eq!(final_timeout_ms(1, 0, 24_000, 0), 25_000);
        assert_eq!(final_timeout_ms(2, 48_001, 24_000, 4), 44_000);
        assert_eq!(
            final_timeout_ms(usize::MAX, usize::MAX, 0, u64::MAX),
            300_000
        );
    }

    #[test]
    fn node_base64_permits_unpadded_url_alphabet_and_noise() {
        assert_eq!(decode_base64(" aG V\nsbG8"), Bytes::from_static(b"hello"));
        assert_eq!(decode_base64("-_8="), Bytes::from_static(&[251, 255]));
        assert_eq!(decode_base64("YQ==Yg=="), Bytes::from_static(b"a"));
        assert!(decode_base64("!").is_empty());
    }

    #[test]
    fn segment_join_preserves_internal_empty_segments() {
        assert_eq!(
            ordered_text(["", "first", "", "third", ""].into_iter()),
            "first  third"
        );
        assert_eq!(
            ordered_text(["  first ", " second  "].into_iter()),
            "first   second"
        );
        assert_eq!(ordered_text(std::iter::empty()), "");
    }

    #[test]
    fn source_numeric_prefix_and_filename_rules() {
        assert_eq!(parse_float_prefix("  +1.5e1 seconds"), Some(15.0));
        assert_eq!(parse_float_prefix("1e+"), Some(1.0));
        assert_eq!(parse_integer_prefix("  -300.5"), Some(-300.0));
        assert_eq!(parse_integer_prefix("invalid"), None);
        assert_eq!(sanitize_filename("a/\u{1f600}b", "session"), "a___b");
        assert!(is_buffer_too_small("Error: BUFFER TOO SMALL."));
        assert_eq!(ordered_text(["\u{feff}hello\u{feff}"].into_iter()), "hello");
        assert_eq!(ordered_text(["\u{0085}hello"].into_iter()), "\u{0085}hello");
    }
}
