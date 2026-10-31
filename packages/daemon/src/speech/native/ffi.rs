//! Hand-transcribed C layouts from sherpa-onnx v1.12.28, c-api/c-api.h.
//! Do not use these layouts with another version: fields are appended *inside*
//! nested configs between releases. The loader checks the exact version first.
//! Source: https://github.com/k2-fsa/sherpa-onnx/blob/v1.12.28/sherpa-onnx/c-api/c-api.h

use std::ffi::{c_char, c_void};

pub type Handle = *const c_void;
pub type Destroy = unsafe extern "C" fn(Handle);

// Every member is a nullable pointer, integer, float, or another such C struct;
// all-zero initialization is the upstream C API's default-config convention.
macro_rules! config {
    ($name:ident { $($field:ident: $ty:ty),* $(,)? }) => {
        #[repr(C)]
        pub struct $name { $(pub $field: $ty,)* }
        impl Default for $name {
            fn default() -> Self { unsafe { std::mem::zeroed() } }
        }
    };
}

config!(Feature {
    sample_rate: i32,
    feature_dim: i32
});
config!(Transducer { encoder: *const c_char, decoder: *const c_char, joiner: *const c_char });
config!(SingleModel { model: *const c_char });
config!(Whisper {
    encoder: *const c_char, decoder: *const c_char, language: *const c_char,
    task: *const c_char, tail_paddings: i32, enable_token_timestamps: i32,
    enable_segment_timestamps: i32,
});
config!(Canary {
    encoder: *const c_char, decoder: *const c_char, src_lang: *const c_char,
    tgt_lang: *const c_char, use_pnc: i32,
});
config!(FireRed { encoder: *const c_char, decoder: *const c_char });
config!(Moonshine {
    preprocessor: *const c_char, encoder: *const c_char,
    uncached_decoder: *const c_char, cached_decoder: *const c_char,
    merged_decoder: *const c_char,
});
config!(LanguageModel { model: *const c_char, scale: f32 });
config!(SenseVoice { model: *const c_char, language: *const c_char, use_itn: i32 });
config!(FunAsrNano {
    encoder_adaptor: *const c_char, llm: *const c_char, embedding: *const c_char,
    tokenizer: *const c_char, system_prompt: *const c_char, user_prompt: *const c_char,
    max_new_tokens: i32, temperature: f32, top_p: f32, seed: i32,
    language: *const c_char, itn: i32, hotwords: *const c_char,
});
config!(OfflineModel {
    transducer: Transducer, paraformer: SingleModel, nemo_ctc: SingleModel,
    whisper: Whisper, tdnn: SingleModel, tokens: *const c_char,
    num_threads: i32, debug: i32, provider: *const c_char, model_type: *const c_char,
    modeling_unit: *const c_char, bpe_vocab: *const c_char, telespeech_ctc: *const c_char,
    sense_voice: SenseVoice, moonshine: Moonshine, fire_red_asr: FireRed,
    dolphin: SingleModel, zipformer_ctc: SingleModel, canary: Canary,
    wenet_ctc: SingleModel, omnilingual: SingleModel, medasr: SingleModel,
    funasr_nano: FunAsrNano, fire_red_asr_ctc: SingleModel,
});
config!(Homophone { dict_dir: *const c_char, lexicon: *const c_char, rule_fsts: *const c_char });
config!(RecognizerConfig {
    feat_config: Feature, model_config: OfflineModel, lm_config: LanguageModel,
    decoding_method: *const c_char, max_active_paths: i32, hotwords_file: *const c_char,
    hotwords_score: f32, rule_fsts: *const c_char, rule_fars: *const c_char,
    blank_penalty: f32, hr: Homophone,
});

#[repr(C)]
pub struct RecognitionResult {
    pub text: *const c_char,
    pub timestamps: *mut f32,
    pub count: i32,
    pub tokens: *const c_char,
    pub tokens_arr: *const *const c_char,
    pub json: *const c_char,
    pub lang: *const c_char,
    pub emotion: *const c_char,
    pub event: *const c_char,
    pub durations: *mut f32,
    pub ys_log_probs: *mut f32,
    pub segment_timestamps: *const f32,
    pub segment_durations: *const f32,
    pub segment_texts: *const c_char,
    pub segment_texts_arr: *const *const c_char,
    pub segment_count: i32,
}

config!(Vits {
    model: *const c_char, lexicon: *const c_char, tokens: *const c_char,
    data_dir: *const c_char, noise_scale: f32, noise_scale_w: f32,
    length_scale: f32, dict_dir: *const c_char,
});
config!(Matcha {
    acoustic_model: *const c_char, vocoder: *const c_char, lexicon: *const c_char,
    tokens: *const c_char, data_dir: *const c_char, noise_scale: f32,
    length_scale: f32, dict_dir: *const c_char,
});
config!(Kokoro {
    model: *const c_char, voices: *const c_char, tokens: *const c_char,
    data_dir: *const c_char, length_scale: f32, dict_dir: *const c_char,
    lexicon: *const c_char, lang: *const c_char,
});
config!(Kitten {
    model: *const c_char, voices: *const c_char, tokens: *const c_char,
    data_dir: *const c_char, length_scale: f32,
});
config!(Zipvoice {
    tokens: *const c_char, encoder: *const c_char, decoder: *const c_char,
    vocoder: *const c_char, data_dir: *const c_char, lexicon: *const c_char,
    feat_scale: f32, t_shift: f32, target_rms: f32, guidance_scale: f32,
});
config!(Pocket {
    lm_flow: *const c_char, lm_main: *const c_char, encoder: *const c_char,
    decoder: *const c_char, text_conditioner: *const c_char,
    vocab_json: *const c_char, token_scores_json: *const c_char,
    voice_embedding_cache_capacity: i32,
});
config!(TtsModel {
    vits: Vits, num_threads: i32, debug: i32, provider: *const c_char,
    matcha: Matcha, kokoro: Kokoro, kitten: Kitten, zipvoice: Zipvoice, pocket: Pocket,
});
config!(TtsConfig {
    model: TtsModel, rule_fsts: *const c_char, max_num_sentences: i32,
    rule_fars: *const c_char, silence_scale: f32,
});
#[repr(C)]
pub struct Audio {
    pub samples: *const f32,
    pub n: i32,
    pub sample_rate: i32,
}

config!(Silero {
    model: *const c_char, threshold: f32, min_silence_duration: f32,
    min_speech_duration: f32, window_size: i32, max_speech_duration: f32,
});
config!(VadConfig {
    silero_vad: Silero, sample_rate: i32, num_threads: i32,
    provider: *const c_char, debug: i32, ten_vad: Silero,
});

pub type CreateRecognizer = unsafe extern "C" fn(*const RecognizerConfig) -> Handle;
pub type CreateStream = unsafe extern "C" fn(Handle) -> Handle;
pub type AcceptWaveform = unsafe extern "C" fn(Handle, i32, *const f32, i32);
pub type Decode = unsafe extern "C" fn(Handle, Handle);
pub type GetResult = unsafe extern "C" fn(Handle) -> *const RecognitionResult;
pub type DestroyResult = unsafe extern "C" fn(*const RecognitionResult);
pub type CreateTts = unsafe extern "C" fn(*const TtsConfig) -> Handle;
pub type Callback = unsafe extern "C" fn(*const f32, i32, *mut c_void) -> i32;
pub type Generate =
    unsafe extern "C" fn(Handle, *const c_char, i32, f32, Callback, *mut c_void) -> *const Audio;
pub type DestroyAudio = unsafe extern "C" fn(*const Audio);
pub type GetInt = unsafe extern "C" fn(Handle) -> i32;
pub type CreateVad = unsafe extern "C" fn(*const VadConfig, f32) -> Handle;
pub type AcceptVad = unsafe extern "C" fn(Handle, *const f32, i32);

pub struct Api {
    pub create_recognizer: CreateRecognizer,
    pub destroy_recognizer: Destroy,
    pub create_stream: CreateStream,
    pub destroy_stream: Destroy,
    pub accept_waveform: AcceptWaveform,
    pub decode: Decode,
    pub get_result: GetResult,
    pub destroy_result: DestroyResult,
    pub create_tts: CreateTts,
    pub destroy_tts: Destroy,
    pub generate: Generate,
    pub destroy_audio: DestroyAudio,
    pub tts_sample_rate: GetInt,
    pub tts_num_speakers: GetInt,
    pub create_vad: CreateVad,
    pub destroy_vad: Destroy,
    pub accept_vad: AcceptVad,
    pub vad_detected: GetInt,
    pub vad_empty: GetInt,
    pub vad_clear: Destroy,
    pub vad_flush: Destroy,
    pub vad_reset: Destroy,
}

impl Api {
    // SAFETY: caller has checked the exact library version and retains the
    // Library for at least as long as this table and every returned C object.
    pub unsafe fn load(library: &libloading::Library) -> anyhow::Result<Self> {
        unsafe fn symbol<T: Copy>(library: &libloading::Library, name: &[u8]) -> anyhow::Result<T> {
            unsafe { library.get::<T>(name).map(|value| *value) }.map_err(|error| {
                anyhow::anyhow!(
                    "Sherpa 1.12.28 C API symbol {} unavailable: {error}",
                    String::from_utf8_lossy(&name[..name.len() - 1])
                )
            })
        }
        macro_rules! get {
            ($name:literal) => {
                unsafe { symbol(library, concat!($name, "\0").as_bytes())? }
            };
        }
        Ok(Self {
            create_recognizer: get!("SherpaOnnxCreateOfflineRecognizer"),
            destroy_recognizer: get!("SherpaOnnxDestroyOfflineRecognizer"),
            create_stream: get!("SherpaOnnxCreateOfflineStream"),
            destroy_stream: get!("SherpaOnnxDestroyOfflineStream"),
            accept_waveform: get!("SherpaOnnxAcceptWaveformOffline"),
            decode: get!("SherpaOnnxDecodeOfflineStream"),
            get_result: get!("SherpaOnnxGetOfflineStreamResult"),
            destroy_result: get!("SherpaOnnxDestroyOfflineRecognizerResult"),
            create_tts: get!("SherpaOnnxCreateOfflineTts"),
            destroy_tts: get!("SherpaOnnxDestroyOfflineTts"),
            generate: get!("SherpaOnnxOfflineTtsGenerateWithCallbackWithArg"),
            destroy_audio: get!("SherpaOnnxDestroyOfflineTtsGeneratedAudio"),
            tts_sample_rate: get!("SherpaOnnxOfflineTtsSampleRate"),
            tts_num_speakers: get!("SherpaOnnxOfflineTtsNumSpeakers"),
            create_vad: get!("SherpaOnnxCreateVoiceActivityDetector"),
            destroy_vad: get!("SherpaOnnxDestroyVoiceActivityDetector"),
            accept_vad: get!("SherpaOnnxVoiceActivityDetectorAcceptWaveform"),
            vad_detected: get!("SherpaOnnxVoiceActivityDetectorDetected"),
            vad_empty: get!("SherpaOnnxVoiceActivityDetectorEmpty"),
            vad_clear: get!("SherpaOnnxVoiceActivityDetectorClear"),
            vad_flush: get!("SherpaOnnxVoiceActivityDetectorFlush"),
            vad_reset: get!("SherpaOnnxVoiceActivityDetectorReset"),
        })
    }
}
