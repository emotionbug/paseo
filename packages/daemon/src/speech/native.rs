//! Native local speech, using the pinned sherpa-onnx 1.12.28 C ABI.
//!
//! `SherpaLibrary::load` loads a trusted, application-installed shared library;
//! it never downloads or launches a worker. Constructors and inference run on
//! Tokio's blocking pool. Each engine serializes native access, including close.
//! Dropping/cancelling an inference future cannot release its handle/library
//! while C is running. Parakeet cannot interrupt an in-flight decode; cancellation
//! discards its result. Kokoro additionally stops at native callback boundaries.
//! Call `close().await` to wait for outstanding C work and release model memory.

mod ffi;

use anyhow::{Context, Result, anyhow, bail, ensure};
use std::{
    ffi::{CStr, CString, c_char, c_void},
    io::Write,
    path::{Path, PathBuf},
    ptr::NonNull,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

pub const SHERPA_VERSION: &str = "1.12.28";
pub const PARAKEET_SAMPLE_RATE: u32 = 16_000;

/// Paths are the model catalog's extracted files, not an archive or a model ID.
#[derive(Clone, Debug)]
pub struct ParakeetConfig {
    pub encoder: PathBuf,
    pub decoder: PathBuf,
    pub joiner: PathBuf,
    pub tokens: PathBuf,
    pub num_threads: i32,
}

impl ParakeetConfig {
    pub fn from_model_dir(dir: PathBuf) -> Self {
        Self {
            encoder: dir.join("encoder.int8.onnx"),
            decoder: dir.join("decoder.int8.onnx"),
            joiner: dir.join("joiner.int8.onnx"),
            tokens: dir.join("tokens.txt"),
            num_threads: 1,
        }
    }
}

#[derive(Clone, Debug)]
pub struct KokoroConfig {
    pub model_dir: PathBuf,
    pub speaker_id: i32,
    pub speed: f32,
    pub length_scale: f32,
    pub num_threads: i32,
}

impl KokoroConfig {
    pub fn from_model_dir(model_dir: PathBuf) -> Self {
        Self {
            model_dir,
            speaker_id: 0,
            speed: 1.0,
            length_scale: 1.0,
            num_threads: 2,
        }
    }
}

/// Native detector settings. The higher-level turn state machine separately
/// confirms speech for 800ms and silence for 1000ms, as in silero-vad-session.ts.
#[derive(Clone, Debug)]
pub struct SileroConfig {
    /// None uses the exact bundled source asset, embedded in the executable and
    /// materialized to a private temporary file retained until the detector dies.
    pub model_path: Option<PathBuf>,
    pub sample_rate: u32,
    pub threshold: f32,
    pub window_size: usize,
    pub buffer_size_seconds: f32,
}

impl Default for SileroConfig {
    fn default() -> Self {
        Self {
            model_path: None,
            sample_rate: 16_000,
            threshold: 0.5,
            window_size: 512,
            buffer_size_seconds: 60.0,
        }
    }
}

#[derive(Debug)]
pub struct GeneratedAudio {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
}

/// The library is retained by every handle and by every active blocking call.
/// Only exact version matches are accepted before any config crosses the ABI.
/// There is no upstream struct-size handshake; version + complete required
/// symbol resolution are the ABI checks, not proof against a malicious library.
pub struct SherpaLibrary {
    api: ffi::Api,
    _library: libloading::Library,
    path: PathBuf,
}

impl SherpaLibrary {
    /// Load an explicit trusted path, or discover the library beside the daemon,
    /// in its `lib` directory, then through the platform's normal loader search.
    /// Set PASEO_SHERPA_ONNX_LIBRARY to select an explicit installation path.
    /// Its ONNX Runtime/espeak dependencies must also be loader-accessible.
    pub fn load(path: Option<&Path>) -> Result<Arc<Self>> {
        if let Some(path) = path {
            return Self::load_path(path);
        }
        if let Some(path) = std::env::var_os("PASEO_SHERPA_ONNX_LIBRARY") {
            return Self::load_path(Path::new(&path));
        }
        let name = if cfg!(target_os = "windows") {
            "sherpa-onnx-c-api.dll"
        } else if cfg!(target_os = "macos") {
            "libsherpa-onnx-c-api.dylib"
        } else {
            "libsherpa-onnx-c-api.so"
        };
        let mut paths = Vec::new();
        if let Ok(executable) = std::env::current_exe() {
            if let Some(dir) = executable.parent() {
                paths.push(dir.join(name));
                paths.push(dir.join("lib").join(name));
                paths.push(dir.join("..").join("lib").join(name));
            }
        }
        paths.push(PathBuf::from(name));
        let mut errors = Vec::new();
        for path in paths {
            match Self::load_path(&path) {
                Ok(library) => return Ok(library),
                Err(error) => errors.push(format!("{}: {error:#}", path.display())),
            }
        }
        bail!(
            "Local speech requires sherpa-onnx {SHERPA_VERSION} C API with TTS enabled for {}-{}. Install the native shared library and its ONNX Runtime dependencies, or set PASEO_SHERPA_ONNX_LIBRARY to its path. Attempts: {}",
            std::env::consts::OS,
            std::env::consts::ARCH,
            errors.join("; ")
        )
    }

    fn load_path(path: &Path) -> Result<Arc<Self>> {
        #[cfg(windows)]
        let library = {
            use libloading::os::windows::{
                LOAD_LIBRARY_SEARCH_DEFAULT_DIRS, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR, Library,
            };
            // DLL_LOAD_DIR requires a fully qualified path. Resolve packaged and
            // operator-selected files before loading so their sibling runtime DLLs
            // participate in dependency resolution without mutating process PATH.
            let qualified = path.is_absolute()
                || path
                    .parent()
                    .is_some_and(|parent| !parent.as_os_str().is_empty())
                || path.is_file();
            let absolute;
            let (filename, flags) = if qualified {
                absolute = std::fs::canonicalize(path)
                    .with_context(|| format!("Cannot resolve Sherpa C API {}", path.display()))?;
                (
                    absolute.as_path(),
                    LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_DEFAULT_DIRS,
                )
            } else {
                (path, LOAD_LIBRARY_SEARCH_DEFAULT_DIRS)
            };
            // SAFETY: the operator controls library selection. Restrict dependency
            // search to its own directory and Windows' configured default directories.
            let library =
                unsafe { Library::load_with_flags(filename, flags) }.with_context(|| {
                    format!(
                        "Cannot load Sherpa C API {} (check architecture and sibling runtime DLLs)",
                        path.display()
                    )
                })?;
            libloading::Library::from(library)
        };
        #[cfg(not(windows))]
        // SAFETY: library selection is an operator-controlled deployment choice,
        // never an untrusted client-provided model name or downloaded script.
        let library = unsafe { libloading::Library::new(path) }.with_context(|| {
            format!(
                "Cannot load Sherpa C API {} (check architecture and dependent libraries)",
                path.display()
            )
        })?;
        let version = unsafe {
            let get_version = library.get::<unsafe extern "C" fn() -> *const c_char>(b"SherpaOnnxGetVersionStr\0")
                .context("Native speech library lacks SherpaOnnxGetVersionStr; expected sherpa-onnx C API 1.12.28")?;
            let raw = get_version();
            ensure!(!raw.is_null(), "Sherpa C API returned a null version");
            CStr::from_ptr(raw)
                .to_str()
                .context("Sherpa C API version is not UTF-8")?
        };
        validate_version(version)?;
        let api = unsafe { ffi::Api::load(&library) }?;
        Ok(Arc::new(Self {
            api,
            _library: library,
            path: path.to_path_buf(),
        }))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub async fn create_parakeet(
        self: &Arc<Self>,
        config: ParakeetConfig,
    ) -> Result<Arc<Parakeet>> {
        let library = self.clone();
        tokio::task::spawn_blocking(move || {
            ensure!(
                config.num_threads > 0,
                "Parakeet numThreads must be positive"
            );
            let encoder = model_file(&config.encoder, "offline encoder")?;
            let decoder = model_file(&config.decoder, "offline decoder")?;
            let joiner = model_file(&config.joiner, "offline joiner")?;
            let tokens = model_file(&config.tokens, "tokens")?;
            let mut native = ffi::RecognizerConfig::default();
            native.feat_config = ffi::Feature {
                sample_rate: 16_000,
                feature_dim: 80,
            };
            native.model_config.transducer = ffi::Transducer {
                encoder: encoder.as_ptr(),
                decoder: decoder.as_ptr(),
                joiner: joiner.as_ptr(),
            };
            native.model_config.tokens = tokens.as_ptr();
            native.model_config.num_threads = config.num_threads;
            native.model_config.provider = c"cpu".as_ptr();
            native.model_config.model_type = c"nemo_transducer".as_ptr();
            native.decoding_method = c"greedy_search".as_ptr();
            native.max_active_paths = 4;
            // The C constructor copies config strings into its C++ config.
            let pointer = unsafe { (library.api.create_recognizer)(&native) };
            let destroy = library.api.destroy_recognizer;
            let handle = NativeHandle::new(pointer, library, destroy, "Parakeet recognizer")?;
            Ok(Arc::new(Parakeet {
                state: Shared::new(handle),
            }))
        })
        .await
        .context("Parakeet initialization task failed")?
    }

    pub async fn create_kokoro(self: &Arc<Self>, config: KokoroConfig) -> Result<Arc<Kokoro>> {
        let library = self.clone();
        tokio::task::spawn_blocking(move || {
            ensure!(config.num_threads > 0, "Kokoro numThreads must be positive");
            ensure!(
                config.speaker_id >= 0,
                "Kokoro speakerId must be nonnegative"
            );
            ensure!(
                config.speed.is_finite() && config.speed > 0.0,
                "Kokoro speed must be positive and finite"
            );
            ensure!(
                config.length_scale.is_finite() && config.length_scale > 0.0,
                "Kokoro lengthScale must be positive and finite"
            );
            let model = model_file(&config.model_dir.join("model.onnx"), "TTS model")?;
            let voices = model_file(&config.model_dir.join("voices.bin"), "TTS voices")?;
            let tokens = model_file(&config.model_dir.join("tokens.txt"), "TTS tokens")?;
            let data_path = config.model_dir.join("espeak-ng-data");
            ensure!(
                data_path.is_dir(),
                "Missing TTS espeak-ng dataDir: {}. Reinstall the complete Kokoro model archive",
                data_path.display()
            );
            let data_dir = path_cstring(&data_path)?;
            let mut native = ffi::TtsConfig::default();
            native.model.num_threads = config.num_threads;
            native.model.provider = c"cpu".as_ptr();
            native.model.kokoro = ffi::Kokoro {
                model: model.as_ptr(),
                voices: voices.as_ptr(),
                tokens: tokens.as_ptr(),
                data_dir: data_dir.as_ptr(),
                length_scale: config.length_scale,
                ..Default::default()
            };
            native.max_num_sentences = 1;
            let pointer = unsafe { (library.api.create_tts)(&native) };
            let destroy = library.api.destroy_tts;
            let handle = NativeHandle::new(pointer, library, destroy, "Kokoro TTS")?;
            let sample_rate = unsafe { (handle.library.api.tts_sample_rate)(handle.pointer()) };
            let speakers = unsafe { (handle.library.api.tts_num_speakers)(handle.pointer()) };
            ensure!(
                sample_rate > 0,
                "Kokoro returned an invalid sample rate: {sample_rate}"
            );
            ensure!(
                config.speaker_id < speakers,
                "Kokoro speakerId {} is outside this model's {speakers} voices",
                config.speaker_id
            );
            Ok(Arc::new(Kokoro {
                state: Shared::new(handle),
                speaker_id: config.speaker_id,
                speed: config.speed,
                sample_rate: sample_rate as u32,
            }))
        })
        .await
        .context("Kokoro initialization task failed")?
    }

    pub async fn create_silero(self: &Arc<Self>, config: SileroConfig) -> Result<Arc<Silero>> {
        let library = self.clone();
        tokio::task::spawn_blocking(move || {
            ensure!(
                config.sample_rate == 16_000 || config.sample_rate == 8_000,
                "Silero sample rate must be 16000 or 8000 Hz"
            );
            ensure!(
                config.window_size > 0 && config.window_size <= i32::MAX as usize,
                "Silero window size must be a positive int32"
            );
            ensure!(
                config.threshold.is_finite() && config.threshold > 0.0 && config.threshold < 1.0,
                "Silero threshold must be between zero and one"
            );
            ensure!(
                config.buffer_size_seconds.is_finite() && config.buffer_size_seconds > 0.0,
                "Silero buffer duration must be positive and finite"
            );
            let mut model_file_owner = None;
            let model_path = match &config.model_path {
                Some(path) => path.clone(),
                None => {
                    let mut file = tempfile::Builder::new()
                        .prefix("paseo-silero-")
                        .suffix(".onnx")
                        .tempfile()
                        .context("Cannot create private temporary file for bundled Silero model")?;
                    file.write_all(include_bytes!(concat!(
                        env!("CARGO_MANIFEST_DIR"),
                        "/assets/silero_vad.onnx"
                    )))
                    .context("Cannot materialize bundled Silero model")?;
                    file.flush()?;
                    let path = file.path().to_path_buf();
                    model_file_owner = Some(file);
                    path
                }
            };
            let model = model_file(&model_path, "Silero VAD model")?;
            let native = ffi::VadConfig {
                silero_vad: ffi::Silero {
                    model: model.as_ptr(),
                    threshold: config.threshold,
                    min_silence_duration: 0.2,
                    min_speech_duration: 0.1,
                    window_size: config.window_size as i32,
                    max_speech_duration: 20.0,
                },
                sample_rate: config.sample_rate as i32,
                num_threads: 1,
                provider: c"cpu".as_ptr(),
                ..Default::default()
            };
            let pointer = unsafe { (library.api.create_vad)(&native, config.buffer_size_seconds) };
            let destroy = library.api.destroy_vad;
            let mut handle = NativeHandle::new(pointer, library, destroy, "Silero VAD")?;
            handle.model_file = model_file_owner;
            Ok(Arc::new(Silero {
                state: Shared::new(VadState {
                    handle,
                    pending: Vec::with_capacity(config.window_size),
                    window_size: config.window_size,
                }),
                sample_rate: config.sample_rate,
                window_size: config.window_size,
            }))
        })
        .await
        .context("Silero initialization task failed")?
    }
}

fn validate_version(version: &str) -> Result<()> {
    ensure!(
        version == SHERPA_VERSION,
        "Sherpa C API ABI mismatch: expected exactly {SHERPA_VERSION}, found {version}. Install the pinned native library; other releases have incompatible nested configuration layouts"
    );
    Ok(())
}

fn path_cstring(path: &Path) -> Result<CString> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        CString::new(path.as_os_str().as_bytes()).context("Native model path contains a NUL byte")
    }
    #[cfg(not(unix))]
    {
        CString::new(
            path.to_str()
                .context("Native model path is not valid UTF-8")?,
        )
        .context("Native model path contains a NUL byte")
    }
}

fn model_file(path: &Path, label: &str) -> Result<CString> {
    let metadata = std::fs::metadata(path).with_context(|| {
        format!(
            "Missing {label}: {}. Install or repair the configured local speech model",
            path.display()
        )
    })?;
    ensure!(
        metadata.is_file() && metadata.len() > 0,
        "Invalid {label}: {} (expected a nonempty model file)",
        path.display()
    );
    path_cstring(path)
}

struct NativeHandle {
    pointer: NonNull<c_void>,
    library: Arc<SherpaLibrary>,
    destroy: ffi::Destroy,
    model_file: Option<tempfile::NamedTempFile>,
}

// Sherpa objects are not reentrant, but can be used on different OS threads.
// Exclusive access is enforced by Shared's owned mutex guard, including Drop.
unsafe impl Send for NativeHandle {}

impl NativeHandle {
    fn new(
        pointer: ffi::Handle,
        library: Arc<SherpaLibrary>,
        destroy: ffi::Destroy,
        label: &str,
    ) -> Result<Self> {
        let pointer = NonNull::new(pointer.cast_mut()).ok_or_else(|| anyhow!(
            "Sherpa failed to create {label}. Check model files, ONNX Runtime compatibility, and native diagnostic output"))?;
        Ok(Self {
            pointer,
            library,
            destroy,
            model_file: None,
        })
    }
    fn pointer(&self) -> ffi::Handle {
        self.pointer.as_ptr().cast_const()
    }
}

impl Drop for NativeHandle {
    fn drop(&mut self) {
        // Library and temporary model file are still alive here; all inference
        // guards have relinquished exclusive ownership before destruction.
        unsafe { (self.destroy)(self.pointer()) }
    }
}

/// Mutex ownership moves into the blocking task rather than staying in the
/// awaiting future. Tokio abort/drop therefore never unlocks an active C call.
struct Shared<T> {
    state: Arc<Mutex<Option<T>>>,
    closed: AtomicBool,
}

impl<T: Send + 'static> Shared<T> {
    fn new(value: T) -> Arc<Self> {
        Arc::new(Self {
            state: Arc::new(Mutex::new(Some(value))),
            closed: AtomicBool::new(false),
        })
    }

    async fn call<R: Send + 'static>(
        self: &Arc<Self>,
        cancel: CancellationToken,
        operation: impl FnOnce(&mut T, &CancellationToken) -> Result<R> + Send + 'static,
    ) -> Result<R> {
        let cancel = cancel.child_token();
        let _cancel_on_drop = cancel.clone().drop_guard();
        ensure!(
            !self.closed.load(Ordering::Acquire),
            "Local speech provider closed"
        );
        let mut guard = tokio::select! {
            biased;
            _ = cancel.cancelled() => bail!("Local speech operation cancelled"),
            guard = self.state.clone().lock_owned() => guard,
        };
        ensure!(
            !self.closed.load(Ordering::Acquire),
            "Local speech provider closed"
        );
        let shared = self.clone();
        let worker_cancel = cancel.clone();
        let worker = tokio::task::spawn_blocking(move || {
            ensure!(
                !worker_cancel.is_cancelled(),
                "Local speech operation cancelled"
            );
            ensure!(
                !shared.closed.load(Ordering::Acquire),
                "Local speech provider closed"
            );
            let value = guard.as_mut().context("Local speech provider closed")?;
            let result = operation(value, &worker_cancel)?;
            ensure!(
                !worker_cancel.is_cancelled(),
                "Local speech operation cancelled"
            );
            Ok(result)
        });
        tokio::select! {
            biased;
            _ = cancel.cancelled() => bail!("Local speech operation cancelled"),
            result = worker => result.context("Native speech operation task failed")?,
        }
    }

    async fn close(self: &Arc<Self>) -> Result<()> {
        self.closed.store(true, Ordering::Release);
        let shared = self.clone();
        // Spawn before awaiting so a dropped close future still drains/releases.
        tokio::task::spawn_blocking(move || {
            let value = shared.state.blocking_lock().take();
            drop(value);
        })
        .await
        .context("Native speech close task failed")
    }
}

pub struct Parakeet {
    state: Arc<Shared<NativeHandle>>,
}

impl Parakeet {
    pub fn sample_rate(&self) -> u32 {
        PARAKEET_SAMPLE_RATE
    }

    /// Input is mono float PCM. Gain normalization, silence filtering, WAV
    /// parsing and source-compatible resampling belong to the provider wrapper.
    pub async fn transcribe(
        self: &Arc<Self>,
        samples: Vec<f32>,
        sample_rate: u32,
        cancel: CancellationToken,
    ) -> Result<String> {
        ensure!(
            sample_rate > 0 && sample_rate <= i32::MAX as u32,
            "Invalid recognition sample rate"
        );
        let n = i32::try_from(samples.len())
            .context("Parakeet audio exceeds the native int32 sample limit")?;
        ensure!(n > 0, "Cannot recognize empty audio");
        self.state
            .call(cancel, move |handle, cancel| {
                let api = &handle.library.api;
                let stream_ptr = unsafe { (api.create_stream)(handle.pointer()) };
                let stream = NativeHandle::new(
                    stream_ptr,
                    handle.library.clone(),
                    api.destroy_stream,
                    "Parakeet stream",
                )?;
                unsafe {
                    (api.accept_waveform)(stream.pointer(), sample_rate as i32, samples.as_ptr(), n)
                };
                ensure!(!cancel.is_cancelled(), "Local speech operation cancelled");
                unsafe { (api.decode)(handle.pointer(), stream.pointer()) };
                ensure!(!cancel.is_cancelled(), "Local speech operation cancelled");
                let result = unsafe { (api.get_result)(stream.pointer()) };
                ensure!(
                    !result.is_null(),
                    "Sherpa Parakeet returned no recognition result"
                );
                let owned = RecognitionResult {
                    pointer: result,
                    destroy: api.destroy_result,
                };
                let text = unsafe { (*owned.pointer).text };
                ensure!(
                    !text.is_null(),
                    "Sherpa Parakeet returned no transcript text"
                );
                Ok(unsafe { CStr::from_ptr(text) }
                    .to_str()
                    .context("Sherpa Parakeet returned invalid UTF-8")?
                    .trim()
                    .to_owned())
            })
            .await
    }

    pub async fn close(&self) -> Result<()> {
        self.state.close().await
    }
}

struct RecognitionResult {
    pointer: *const ffi::RecognitionResult,
    destroy: ffi::DestroyResult,
}
impl Drop for RecognitionResult {
    fn drop(&mut self) {
        unsafe { (self.destroy)(self.pointer) }
    }
}

pub struct Kokoro {
    state: Arc<Shared<NativeHandle>>,
    speaker_id: i32,
    speed: f32,
    sample_rate: u32,
}

impl Kokoro {
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub async fn synthesize(
        self: &Arc<Self>,
        text: String,
        cancel: CancellationToken,
    ) -> Result<GeneratedAudio> {
        let text = text.trim();
        ensure!(!text.is_empty(), "Cannot synthesize empty text");
        let text = CString::new(text).context("Cannot synthesize text containing a NUL byte")?;
        let speaker_id = self.speaker_id;
        let speed = self.speed;
        self.state
            .call(cancel, move |handle, cancel| {
                let api = &handle.library.api;
                // The C call invokes callbacks synchronously and never retains arg.
                // The borrowed token and text remain alive until generation returns.
                let audio = unsafe {
                    (api.generate)(
                        handle.pointer(),
                        text.as_ptr(),
                        speaker_id,
                        speed,
                        synthesis_callback,
                        (cancel as *const CancellationToken).cast_mut().cast(),
                    )
                };
                ensure!(
                    !audio.is_null(),
                    "Sherpa Kokoro returned no generated audio"
                );
                let owned = GeneratedResult {
                    pointer: audio,
                    destroy: api.destroy_audio,
                };
                ensure!(!cancel.is_cancelled(), "Local speech operation cancelled");
                let audio = unsafe { &*owned.pointer };
                ensure!(
                    audio.n >= 0 && audio.sample_rate > 0,
                    "Sherpa Kokoro returned invalid sample count/rate"
                );
                ensure!(
                    audio.n == 0 || !audio.samples.is_null(),
                    "Sherpa Kokoro returned no Float32 samples"
                );
                let samples = if audio.n == 0 {
                    Vec::new()
                } else {
                    // Exactly one copy is required before freeing Sherpa's allocation.
                    unsafe { std::slice::from_raw_parts(audio.samples, audio.n as usize) }.to_vec()
                };
                Ok(GeneratedAudio {
                    samples,
                    sample_rate: audio.sample_rate as u32,
                })
            })
            .await
    }

    pub async fn close(&self) -> Result<()> {
        self.state.close().await
    }
}

unsafe extern "C" fn synthesis_callback(_samples: *const f32, _n: i32, arg: *mut c_void) -> i32 {
    // No allocation, panic, or user callback is allowed across this C boundary.
    i32::from(!unsafe { &*arg.cast::<CancellationToken>() }.is_cancelled())
}

struct GeneratedResult {
    pointer: *const ffi::Audio,
    destroy: ffi::DestroyAudio,
}
impl Drop for GeneratedResult {
    fn drop(&mut self) {
        unsafe { (self.destroy)(self.pointer) }
    }
}

struct VadState {
    handle: NativeHandle,
    pending: Vec<f32>,
    window_size: usize,
}

impl VadState {
    fn append(&mut self, mut samples: &[f32], cancel: &CancellationToken) -> Result<Vec<bool>> {
        let mut detected = Vec::with_capacity(
            (self.pending.len() + samples.len()).saturating_sub(1) / self.window_size,
        );
        // Source intentionally leaves a whole window pending: size > window,
        // rather than >= window. Do not introduce an extra detection at equality.
        while self.pending.len() + samples.len() > self.window_size {
            ensure!(!cancel.is_cancelled(), "Local speech operation cancelled");
            let consumed;
            let window = if self.pending.is_empty() {
                consumed = self.window_size;
                &samples[..consumed]
            } else {
                consumed = self.window_size - self.pending.len();
                self.pending.extend_from_slice(&samples[..consumed]);
                &self.pending[..]
            };
            let api = &self.handle.library.api;
            unsafe {
                (api.accept_vad)(
                    self.handle.pointer(),
                    window.as_ptr(),
                    self.window_size as i32,
                )
            };
            detected.push(unsafe { (api.vad_detected)(self.handle.pointer()) } != 0);
            // This wrapper consumes only detection state, not saved segments.
            // Clear completed segments so long sessions do not retain PCM forever.
            if unsafe { (api.vad_empty)(self.handle.pointer()) } == 0 {
                unsafe { (api.vad_clear)(self.handle.pointer()) };
            }
            self.pending.clear();
            samples = &samples[consumed..];
        }
        self.pending.extend_from_slice(samples);
        Ok(detected)
    }
}

pub struct Silero {
    state: Arc<Shared<VadState>>,
    sample_rate: u32,
    window_size: usize,
}

impl Silero {
    pub fn required_sample_rate(&self) -> u32 {
        self.sample_rate
    }
    pub fn window_size(&self) -> usize {
        self.window_size
    }

    /// One bool per complete native window, preserving source's strict >512
    /// buffering. The caller advances its boundary clock per returned bool.
    pub async fn append(
        self: &Arc<Self>,
        samples: Vec<f32>,
        cancel: CancellationToken,
    ) -> Result<Vec<bool>> {
        self.state
            .call(cancel, move |state, cancel| state.append(&samples, cancel))
            .await
    }

    /// Flushes the native detector without submitting the pending partial window,
    /// exactly as the source does. The caller applies/forces final turn events.
    pub async fn flush(self: &Arc<Self>, cancel: CancellationToken) -> Result<bool> {
        self.state
            .call(cancel, |state, _| {
                let api = &state.handle.library.api;
                unsafe { (api.vad_flush)(state.handle.pointer()) };
                let detected = unsafe { (api.vad_detected)(state.handle.pointer()) } != 0;
                if unsafe { (api.vad_empty)(state.handle.pointer()) } == 0 {
                    unsafe { (api.vad_clear)(state.handle.pointer()) };
                }
                Ok(detected)
            })
            .await
    }

    pub async fn reset(&self) -> Result<()> {
        self.state
            .call(CancellationToken::new(), |state, _| {
                unsafe { (state.handle.library.api.vad_reset)(state.handle.pointer()) };
                state.pending.clear();
                Ok(())
            })
            .await
    }

    pub async fn close(&self) -> Result<()> {
        self.state.close().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incompatible_native_version_is_rejected_before_configuration_use() {
        assert!(validate_version("1.12.28").is_ok());
        for version in ["1.12.27", "1.12.29", "1.12.28-dev", "", "v1.12.28"] {
            assert!(validate_version(version).is_err());
        }
    }

    #[tokio::test]
    async fn cancelled_call_retains_exclusive_handle_until_native_return() {
        let state = Shared::new(7u32);
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let cancel = CancellationToken::new();
        let caller_state = state.clone();
        let caller_cancel = cancel.clone();
        let caller = tokio::spawn(async move {
            caller_state
                .call(caller_cancel, move |value, _| {
                    let _ = entered_tx.send(());
                    release_rx.recv().unwrap();
                    *value += 1;
                    Ok(())
                })
                .await
        });
        entered_rx.await.unwrap();
        cancel.cancel();
        assert!(caller.await.unwrap().is_err());
        assert!(state.state.try_lock().is_err());
        release_tx.send(()).unwrap();
        state.close().await.unwrap();
        assert!(state.state.lock().await.is_none());
        assert!(
            state
                .call(CancellationToken::new(), |_, _| Ok(()))
                .await
                .is_err()
        );
    }
}
