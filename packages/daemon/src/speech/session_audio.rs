use super::{SpeechService, audio};
use anyhow::{Result, bail};
use bytes::Bytes;
use futures_util::{StreamExt, stream::FuturesUnordered};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeMap as Environment},
    path::PathBuf,
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;

pub async fn transcribe_batch(
    service: &SpeechService,
    bytes: Bytes,
    format: &str,
    environment: &Environment<String, String>,
    cancel: &CancellationToken,
) -> Result<Value> {
    let started = Instant::now();
    let rate = service.required_sample_rate(false).await?;
    let (input_rate, pcm) = if format.to_ascii_lowercase().contains("pcm") {
        (
            audio::parse_pcm_rate_from_format(format, Some(rate)).unwrap_or(rate),
            bytes.as_ref(),
        )
    } else if format.to_ascii_lowercase().contains("wav") {
        let wav = audio::parse_pcm16_mono_wav(&bytes)?;
        (wav.sample_rate, wav.pcm16)
    } else {
        bail!("Unsupported audio format for STT: {format}")
    };
    let pcm = if input_rate == rate {
        let offset = pcm.as_ptr() as usize - bytes.as_ptr() as usize;
        bytes.slice(offset..offset + pcm.len())
    } else {
        Bytes::from(audio::Pcm16MonoResampler::new(input_rate, rate)?.process_chunk(pcm)?)
    };
    let seconds = environment
        .get("PASEO_STT_BATCH_COMMIT_EVERY_SECONDS")
        .and_then(|value| parse_float_prefix(value))
        .filter(|v| v.is_finite() && *v >= 0.0)
        .unwrap_or(15.0);
    let append = rate as usize * 2;
    let commit = if seconds > 0.0 {
        (rate as f64 * 2.0 * seconds).round().max(1.0) as usize
    } else {
        0
    };
    let token = cancel.child_token();
    let _cancel_on_drop = token.clone().drop_guard();
    let mut segments = Vec::new();
    let mut start = 0;
    for end in (append..pcm.len())
        .step_by(append)
        .chain(std::iter::once(pcm.len()))
    {
        if end > start && commit > 0 && end - start >= commit {
            segments.push(pcm.slice(start..end));
            start = end;
        }
    }
    if start < pcm.len() || segments.is_empty() {
        segments.push(pcm.slice(start..));
    }
    let pcm_format = format!("audio/pcm;rate={rate}");
    let mut pending = FuturesUnordered::new();
    for (index, segment) in segments.into_iter().enumerate() {
        let token = token.clone();
        let format = &pcm_format;
        pending.push(async move {
            Ok::<_, anyhow::Error>((
                index,
                service.transcribe(false, segment, format, &token).await?,
            ))
        });
    }
    let deadline = tokio::time::sleep(Duration::from_secs(120));
    tokio::pin!(deadline);
    let mut completed = BTreeMap::new();
    loop {
        tokio::select! {biased;
            _=cancel.cancelled()=>bail!("Speech operation cancelled"),
            _=&mut deadline=>{tracing::warn!(received_finals=completed.len(),"Timed out waiting for final STT segments; returning available transcripts");break},
            next=pending.next()=>match next {Some(result)=>{let (index,value)=result?;completed.insert(index,value);},None=>break}
        }
    }
    let text = completed
        .values()
        .map(|v| v["text"].as_str().unwrap_or(""))
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_owned();
    let low = !completed.is_empty() && completed.values().all(|v| v["isLowConfidence"] == true);
    let mut result = json!({"text":if low {""}else{&text},"duration":started.elapsed().as_millis() as u64,"byteLength":bytes.len(),"format":format});
    if let Some(language) = completed
        .values()
        .find_map(|v| v["language"].as_str().filter(|s| !s.is_empty()))
    {
        result["language"] = json!(language);
    }
    if completed.len() == 1 {
        for field in ["logprobs", "avgLogprob"] {
            if let Some(value) = completed.values().next().and_then(|v| v.get(field)) {
                result[field] = value.clone();
            }
        }
    }
    if low {
        result["isLowConfidence"] = json!(true);
    }
    Ok(result)
}

pub async fn persist(
    environment: &Environment<String, String>,
    key: &str,
    session: &str,
    parts: &[&str],
    format: &str,
    bytes: &[u8],
) -> Result<Option<PathBuf>> {
    let explicit = environment
        .get(key)
        .map(|v| v.trim())
        .filter(|v| !v.is_empty());
    let enabled = environment.get("PASEO_DICTATION_DEBUG").is_some_and(|v| {
        matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    });
    if explicit.is_none() && !enabled {
        return Ok(None);
    }
    let root = explicit
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(".debug/recordings"));
    let root = if root.is_absolute() {
        root
    } else {
        std::env::current_dir()?.join(root)
    };
    let folder = root.join(sanitize(session, "session"));
    tokio::fs::create_dir_all(&folder).await?;
    let timestamp = chrono::Utc::now()
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
        .replace([':', '.'], "-");
    let mut name = timestamp;
    for part in parts {
        if !part.is_empty() {
            name.push('_');
            name.push_str(&sanitize(part, "source"));
        }
    }
    let normalized = format.to_ascii_lowercase();
    let extension = [
        ("webm", "webm"),
        ("ogg", "ogg"),
        ("mp3", "mp3"),
        ("wav", "wav"),
        ("m4a", "m4a"),
        ("aac", "m4a"),
        ("mp4", "mp4"),
        ("flac", "flac"),
    ]
    .into_iter()
    .find(|(needle, _)| normalized.contains(needle))
    .map(|(_, ext)| ext)
    .unwrap_or("webm");
    let path = folder.join(format!("{name}.{extension}"));
    tokio::fs::write(&path, bytes).await?;
    Ok(Some(path))
}
fn sanitize(value: &str, fallback: &str) -> String {
    let value = if value.is_empty() { fallback } else { value };
    value
        .encode_utf16()
        .take(64)
        .map(|unit| {
            if unit < 128
                && ((unit as u8).is_ascii_alphanumeric()
                    || unit == b'-' as u16
                    || unit == b'_' as u16)
            {
                unit as u8 as char
            } else {
                '_'
            }
        })
        .collect()
}
fn parse_float_prefix(value: &str) -> Option<f64> {
    let value = value.trim_start();
    for end in (1..=value.len()).rev() {
        if value.is_char_boundary(end) {
            if let Ok(number) = value[..end].parse::<f64>() {
                return Some(number);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn filename_uses_js_utf16_units() {
        assert_eq!(sanitize("x🦀/y", "file"), "x___y");
        assert_eq!(sanitize("", "file"), "file");
    }
    #[test]
    fn batch_config_keeps_source_parse_float_prefix() {
        assert_eq!(parse_float_prefix(" 2.5seconds"), Some(2.5));
        assert_eq!(parse_float_prefix("not a number"), None);
    }
}
