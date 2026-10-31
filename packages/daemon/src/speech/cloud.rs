//! OpenAI-compatible REST speech. Only endpoint credentials determine availability;
//! model and voice identifiers belong to the configured server and retain their case.
use anyhow::{Context, Result, anyhow, bail};
use bytes::Bytes;
use reqwest::{Client, Response, StatusCode, Url, header::HeaderMap, multipart};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";
pub const DEFAULT_OPENAI_TTS_MODEL: &str = "tts-1";
/// The source OpenAI SDK client default (`maxRetries: 2`).
const OPENAI_MAX_RETRIES: u32 = 2;

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SttConfig {
    api_key: String,
    base_url: Option<String>,
    model: Option<String>,
    confidence_threshold: Option<f64>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TtsConfig {
    api_key: String,
    base_url: Option<String>,
    model: Option<String>,
    voice: Option<String>,
    response_format: Option<String>,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Availability {
    pub stt: bool,
    pub tts: bool,
    pub dictation_stt: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptionResult {
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    pub duration: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logprobs: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avg_logprob: Option<f64>,
    pub is_low_confidence: bool,
}

pub struct SpeechAudio {
    pub bytes: Bytes,
    pub format: String,
}

/// A successful synthesis response whose body has not been read yet.
pub struct SpeechStream {
    pub format: String,
    response: Option<Response>,
}

#[derive(Clone)]
pub struct CloudSpeech {
    client: Client,
    stt: Option<SttConfig>,
    tts: Option<TtsConfig>,
}

impl CloudSpeech {
    /// Accepts exactly the resolved `openai` config object, not the server snapshot.
    /// A null object represents no configured cloud endpoints.
    pub fn from_resolved(config: &Value) -> Result<Self> {
        let stt = config
            .get("stt")
            .filter(|value| {
                value
                    .get("apiKey")
                    .and_then(Value::as_str)
                    .is_some_and(|key| !key.is_empty())
            })
            .map(SttConfig::deserialize)
            .transpose()?;
        let tts = config
            .get("tts")
            .filter(|value| {
                value
                    .get("apiKey")
                    .and_then(Value::as_str)
                    .is_some_and(|key| !key.is_empty())
            })
            .map(TtsConfig::deserialize)
            .transpose()?;
        // Matches the OpenAI client's request timeout. Requests are not retried after
        // ambiguous effects; cancellation drops both pending headers and body reads.
        let client = Client::builder()
            .timeout(Duration::from_secs(600))
            .build()?;
        Ok(Self { client, stt, tts })
    }

    pub fn availability(&self) -> Availability {
        Availability {
            stt: self.stt.is_some(),
            tts: self.tts.is_some(),
            dictation_stt: self.stt.is_some(),
        }
    }

    /// Caller may log this source-shaped warning list; absent credentials do not fail startup.
    pub fn missing_credential_features(&self, providers: &Value) -> Vec<&'static str> {
        let mut missing = Vec::new();
        for (field, name, available) in [
            ("voiceStt", "voice.stt", self.stt.is_some()),
            ("voiceTts", "voice.tts", self.tts.is_some()),
            ("dictationStt", "dictation.stt", self.stt.is_some()),
        ] {
            if active_openai(&providers[field]) && !available {
                missing.push(name);
            }
        }
        missing
    }

    pub async fn transcribe(
        &self,
        audio: Bytes,
        format: &str,
        language: &str,
        prompt: Option<&str>,
        cancellation: &CancellationToken,
    ) -> Result<TranscriptionResult> {
        let operation = async {
            let config = self
                .stt
                .as_ref()
                .context("OpenAI STT credentials are not configured")?;
            let started = Instant::now();
            let model = config.model.as_deref().unwrap_or("whisper-1");
            let part = multipart::Part::stream(reqwest::Body::from(audio)).file_name(format!(
                "audio-{}.{}",
                uuid::Uuid::new_v4(),
                infer_audio_extension(format)
            ));
            let mut form = multipart::Form::new()
                .part("file", part)
                .text("language", language.to_owned())
                .text("model", model.to_owned())
                .text("response_format", "json");
            if let Some(prompt) = prompt.filter(|prompt| !prompt.is_empty()) {
                form = form.text("prompt", prompt.to_owned());
            }
            if supports_logprobs(model) {
                form = form.text("include[]", "logprobs");
            }
            let response = self
                .client
                .post(endpoint_url(config.base_url.as_deref(), "transcriptions")?)
                .bearer_auth(&config.api_key)
                .multipart(form)
                .send()
                .await?;
            let response = successful_response(response).await?;
            let value = response
                .json::<Value>()
                .await
                .context("Invalid transcription JSON response")?;
            parse_transcription(
                value,
                model,
                config.confidence_threshold.unwrap_or(-3.0),
                started.elapsed().as_millis() as u64,
            )
        };
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => Err(anyhow!("Speech operation cancelled")),
            result = operation => result,
        }
        .map_err(|error| anyhow!("STT transcription failed: {error}"))
    }

    pub async fn synthesize_stream(
        &self,
        text: &str,
        cancellation: &CancellationToken,
    ) -> Result<SpeechStream> {
        if text.trim().is_empty() {
            bail!("Cannot synthesize empty text");
        }
        let operation = async {
            let config = self
                .tts
                .as_ref()
                .context("OpenAI TTS credentials are not configured")?;
            let format = config.response_format.as_deref().unwrap_or("pcm");
            let url = endpoint_url(config.base_url.as_deref(), "speech")?;
            let body = tts_request(config, text);
            let mut retries_remaining = OPENAI_MAX_RETRIES;
            loop {
                let sent = self
                    .client
                    .post(url.clone())
                    .bearer_auth(&config.api_key)
                    .json(&body)
                    .send()
                    .await;
                let delay = match sent {
                    Ok(response)
                        if retries_remaining > 0
                            && should_retry(response.status(), response.headers()) =>
                    {
                        retry_delay(response.headers(), retries_remaining)
                    }
                    Ok(response) => {
                        return Ok::<_, anyhow::Error>(SpeechStream {
                            format: format.to_owned(),
                            response: Some(successful_response(response).await?),
                        });
                    }
                    // The SDK retries connection failures and timeouts as well.
                    Err(error) if retries_remaining > 0 && !error.is_builder() => {
                        retry_delay(&HeaderMap::new(), retries_remaining)
                    }
                    Err(error) => return Err(anyhow::Error::from(error)),
                };
                tokio::time::sleep(delay).await;
                retries_remaining -= 1;
            }
        };
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => Err(anyhow!("Speech operation cancelled")),
            result = operation => result,
        }
        .map_err(|error: anyhow::Error| anyhow!("TTS synthesis failed: {error}"))
    }

    pub async fn synthesize(
        &self,
        text: &str,
        cancellation: &CancellationToken,
    ) -> Result<SpeechAudio> {
        let mut stream = self.synthesize_stream(text, cancellation).await?;
        let response = stream
            .response
            .take()
            .expect("new synthesis stream owns its response");
        let bytes = tokio::select! {
            biased;
            _ = cancellation.cancelled() => bail!("TTS synthesis failed: Speech operation cancelled"),
            bytes = response.bytes() => bytes.context("TTS synthesis failed")?,
        };
        Ok(SpeechAudio {
            bytes,
            format: stream.format,
        })
    }
}

fn active_openai(provider: &Value) -> bool {
    provider.get("enabled").and_then(Value::as_bool) != Some(false)
        && provider.get("provider").and_then(Value::as_str) == Some("openai")
}

fn tts_request(config: &TtsConfig, text: &str) -> Value {
    json!({
        "model": config.model.as_deref().unwrap_or(DEFAULT_OPENAI_TTS_MODEL),
        "voice": config.voice.as_deref().unwrap_or("alloy"),
        "input": text,
        "response_format": config.response_format.as_deref().unwrap_or("pcm"),
    })
}

fn endpoint_url(base: Option<&str>, operation: &str) -> Result<Url> {
    let mut url = Url::parse(
        base.filter(|base| !base.is_empty())
            .unwrap_or(DEFAULT_BASE_URL),
    )?;
    let path = format!("{}/audio/{operation}", url.path().trim_end_matches('/'));
    url.set_path(&path);
    Ok(url)
}

async fn successful_response(response: Response) -> Result<Response> {
    if response.status().is_success() {
        return Ok(response);
    }
    let status = response.status();
    let body = response.text().await?;
    let parsed = serde_json::from_str::<Value>(&body).ok();
    let message = parsed
        .as_ref()
        .and_then(|value| value.pointer("/error/message"))
        .and_then(Value::as_str)
        .unwrap_or(&body);
    bail!("{} {message}", status.as_u16())
}

/// Source OpenAI SDK `shouldRetry`.
fn should_retry(status: StatusCode, headers: &HeaderMap) -> bool {
    match headers
        .get("x-should-retry")
        .and_then(|value| value.to_str().ok())
    {
        Some("true") => return true,
        Some("false") => return false,
        _ => {}
    }
    matches!(status.as_u16(), 408 | 409 | 429) || status.is_server_error()
}

/// Source OpenAI SDK `retryRequest` delay: server hints under one minute, otherwise
/// 0.5s doubling per attempt, capped at 8s, with up to 25% jitter.
fn retry_delay(headers: &HeaderMap, retries_remaining: u32) -> Duration {
    let header = |name: &str| headers.get(name).and_then(|value| value.to_str().ok());
    let mut millis = header("retry-after-ms").and_then(|value| value.trim().parse::<f64>().ok());
    if millis.is_none_or(|millis| millis == 0.0) {
        if let Some(value) = header("retry-after") {
            millis = match value.trim().parse::<f64>() {
                Ok(seconds) => Some(seconds * 1000.0),
                Err(_) => chrono::DateTime::parse_from_rfc2822(value.trim())
                    .ok()
                    .map(|date| (date.timestamp_millis() - chrono::Utc::now().timestamp_millis()) as f64),
            };
        }
    }
    let millis = millis
        .filter(|millis| *millis > 0.0 && *millis < 60_000.0)
        .unwrap_or_else(|| {
            let attempt = OPENAI_MAX_RETRIES - retries_remaining;
            let seconds = (0.5 * 2f64.powi(attempt as i32)).min(8.0);
            seconds * (1.0 - rand::random::<f64>() * 0.25) * 1000.0
        });
    Duration::from_secs_f64(millis / 1000.0)
}

fn supports_logprobs(model: &str) -> bool {
    matches!(model, "gpt-4o-transcribe" | "gpt-4o-mini-transcribe")
}

fn valid_logprobs(value: &Value) -> bool {
    value.as_array().is_some_and(|tokens| {
        tokens.iter().all(|token| {
            token.get("token").is_some_and(Value::is_string)
                && token.get("logprob").is_some_and(Value::is_number)
                && token.get("bytes").is_none_or(|bytes| {
                    bytes
                        .as_array()
                        .is_some_and(|bytes| bytes.iter().all(Value::is_number))
                })
        })
    })
}

fn parse_transcription(
    mut response: Value,
    model: &str,
    threshold: f64,
    duration: u64,
) -> Result<TranscriptionResult> {
    let text = response
        .get_mut("text")
        .and_then(take_string)
        .context("Invalid transcription response: text must be a string")?;
    let language = response.get_mut("language").and_then(take_string);
    let logprobs = response
        .get_mut("logprobs")
        .filter(|tokens| supports_logprobs(model) && valid_logprobs(tokens))
        .map(Value::take);
    let avg_logprob = logprobs
        .as_ref()
        .and_then(Value::as_array)
        .filter(|tokens| !tokens.is_empty())
        .map(|tokens| {
            tokens
                .iter()
                .map(|token| {
                    token["logprob"]
                        .as_f64()
                        .expect("validated numeric logprob")
                })
                .sum::<f64>()
                / tokens.len() as f64
        });
    Ok(TranscriptionResult {
        text,
        language,
        duration,
        logprobs,
        avg_logprob,
        is_low_confidence: avg_logprob.is_some_and(|average| average < threshold),
    })
}

fn take_string(value: &mut Value) -> Option<String> {
    match value.take() {
        Value::String(text) => Some(text),
        _ => None,
    }
}

pub fn infer_audio_extension(format: &str) -> &'static str {
    let format = format.to_lowercase();
    for (needle, extension) in [
        ("webm", "webm"),
        ("ogg", "ogg"),
        ("mp3", "mp3"),
        ("wav", "wav"),
        ("m4a", "m4a"),
        ("aac", "m4a"),
        ("mp4", "mp4"),
        ("flac", "flac"),
    ] {
        if format.contains(needle) {
            return extension;
        }
    }
    "webm"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confidence_fixture_is_model_gated_and_strictly_below_threshold() {
        let fixture = json!({"text":"hello", "language":"en", "logprobs":[
            {"token":"hel", "logprob":-2, "bytes":[104,101,108]},
            {"token":"lo", "logprob":-4, "bytes":[108,111], "extra":"retained"}
        ]});
        let result = parse_transcription(fixture.clone(), "gpt-4o-transcribe", -3.0, 12).unwrap();
        assert_eq!(result.avg_logprob, Some(-3.0));
        assert!(!result.is_low_confidence);
        assert_eq!(result.logprobs.as_ref().unwrap()[1]["extra"], "retained");
        assert!(
            parse_transcription(fixture.clone(), "gpt-4o-mini-transcribe", -2.9, 12)
                .unwrap()
                .is_low_confidence
        );
        let custom = parse_transcription(fixture, "Whisper-Custom:Q8_0", -3.0, 12).unwrap();
        assert!(custom.logprobs.is_none());
        assert!(custom.avg_logprob.is_none());
        assert!(!custom.is_low_confidence);
    }

    #[test]
    fn malformed_token_invalidates_entire_logprob_array() {
        for logprobs in [
            json!([{"token":"ok","logprob":-1},{"token":"bad","logprob":"-9"}]),
            json!([{"token":"a","logprob":-9,"bytes":null}]),
        ] {
            let result = parse_transcription(
                json!({"text":"words","logprobs":logprobs}),
                "gpt-4o-transcribe",
                -3.0,
                0,
            )
            .unwrap();
            assert!(result.logprobs.is_none());
            assert!(!result.is_low_confidence);
        }
        let empty = parse_transcription(
            json!({"text":"", "logprobs":[]}),
            "gpt-4o-transcribe",
            -3.0,
            0,
        )
        .unwrap();
        assert_eq!(empty.logprobs, Some(json!([])));
        assert_eq!(empty.avg_logprob, None);
    }

    #[test]
    fn custom_endpoint_and_case_sensitive_tts_ids() {
        let config: TtsConfig = serde_json::from_value(
            json!({"apiKey":"fixture", "model":"kokoro-v1:Q8_0", "voice":"Aiden"}),
        )
        .unwrap();
        assert_eq!(
            tts_request(&config, " hello "),
            json!({"model":"kokoro-v1:Q8_0", "voice":"Aiden", "input":" hello ", "response_format":"pcm"})
        );
        assert_eq!(
            endpoint_url(Some("https://speech.example.test/custom/v1/"), "speech")
                .unwrap()
                .as_str(),
            "https://speech.example.test/custom/v1/audio/speech"
        );
        assert_eq!(
            endpoint_url(None, "transcriptions").unwrap().as_str(),
            "https://api.openai.com/v1/audio/transcriptions"
        );
    }

    #[test]
    fn source_format_extension_order_and_fallback() {
        assert_eq!(infer_audio_extension("audio/AAC"), "m4a");
        assert_eq!(infer_audio_extension("audio/webm;codecs=opus"), "webm");
        assert_eq!(infer_audio_extension("audio/unknown"), "webm");
    }

    #[test]
    fn tts_retries_follow_the_openai_sdk_policy() {
        let mut headers = HeaderMap::new();
        assert!(should_retry(StatusCode::TOO_MANY_REQUESTS, &headers));
        assert!(should_retry(StatusCode::REQUEST_TIMEOUT, &headers));
        assert!(should_retry(StatusCode::CONFLICT, &headers));
        assert!(should_retry(StatusCode::BAD_GATEWAY, &headers));
        assert!(!should_retry(StatusCode::BAD_REQUEST, &headers));
        assert!(!should_retry(StatusCode::UNAUTHORIZED, &headers));
        let first = retry_delay(&headers, OPENAI_MAX_RETRIES);
        assert!(first >= Duration::from_millis(375) && first <= Duration::from_millis(500));
        let second = retry_delay(&headers, OPENAI_MAX_RETRIES - 1);
        assert!(second >= Duration::from_millis(750) && second <= Duration::from_millis(1000));
        headers.insert("retry-after", "2".parse().unwrap());
        assert_eq!(retry_delay(&headers, 1), Duration::from_secs(2));
        headers.insert("retry-after-ms", "1500".parse().unwrap());
        assert_eq!(retry_delay(&headers, 1), Duration::from_millis(1500));
        headers.insert("retry-after-ms", "90000".parse().unwrap());
        assert!(retry_delay(&headers, 2) <= Duration::from_millis(500));
        headers.insert("x-should-retry", "false".parse().unwrap());
        assert!(!should_retry(StatusCode::SERVICE_UNAVAILABLE, &headers));
        headers.insert("x-should-retry", "true".parse().unwrap());
        assert!(should_retry(StatusCode::BAD_REQUEST, &headers));
    }

    #[tokio::test]
    async fn pre_cancelled_operations_never_open_endpoints() {
        let cloud = CloudSpeech::from_resolved(
            &json!({"stt":{"apiKey":"fixture"},"tts":{"apiKey":"fixture"}}),
        )
        .unwrap();
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let error = cloud
            .transcribe(Bytes::new(), "audio/wav", "en", None, &cancellation)
            .await
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "STT transcription failed: Speech operation cancelled"
        );
        assert!(cloud.synthesize("hello", &cancellation).await.is_err());
    }
}
