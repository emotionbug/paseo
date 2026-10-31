//! PCM helpers shared by cloud and native speech. Resampling follows the server's
//! streaming linear interpolation, including its retained final input sample.
use anyhow::{Result, bail, ensure};
use std::sync::LazyLock;

pub struct Pcm16Wav<'a> {
    pub sample_rate: u32,
    pub pcm16: &'a [u8],
}

pub fn parse_pcm16_mono_wav(buffer: &[u8]) -> Result<Pcm16Wav<'_>> {
    ensure!(
        buffer.get(..4) == Some(b"RIFF") && buffer.get(8..12) == Some(b"WAVE"),
        "Invalid WAV header"
    );
    let mut offset = 12usize;
    let mut format = None;
    let mut data = None;
    while buffer.len().saturating_sub(offset) >= 8 {
        let id = &buffer[offset..offset + 4];
        let size = u32::from_le_bytes(buffer[offset + 4..offset + 8].try_into()?) as usize;
        let start = offset + 8;
        let Some(end) = start.checked_add(size).filter(|end| *end <= buffer.len()) else {
            break;
        };
        if id == b"fmt " {
            ensure!(size >= 16, "Invalid WAV fmt chunk");
            format = Some((
                u16::from_le_bytes(buffer[start..start + 2].try_into()?),
                u16::from_le_bytes(buffer[start + 2..start + 4].try_into()?),
                u32::from_le_bytes(buffer[start + 4..start + 8].try_into()?),
                u16::from_le_bytes(buffer[start + 14..start + 16].try_into()?),
            ));
        } else if id == b"data" {
            data = Some(&buffer[start..end]);
        }
        offset = end + size % 2;
    }
    let (Some((encoding, channels, sample_rate, bits)), Some(pcm16)) = (format, data) else {
        bail!("Missing WAV fmt/data chunks");
    };
    ensure!(
        encoding == 1,
        "Unsupported WAV encoding (audioFormat={encoding})"
    );
    ensure!(
        channels == 1 && bits == 16,
        "Unexpected WAV format: channels={channels} rate={sample_rate} bits={bits}"
    );
    ensure!(pcm16.len() % 2 == 0, "WAV PCM16 data length must be even");
    Ok(Pcm16Wav { sample_rate, pcm16 })
}

pub fn pcm16_mono_wav(pcm16: &[u8], sample_rate: u32) -> Result<Vec<u8>> {
    check_pcm16(pcm16)?;
    let data_size = u32::try_from(pcm16.len())?;
    let riff_size = data_size
        .checked_add(36)
        .ok_or_else(|| anyhow::anyhow!("WAV is too large"))?;
    let byte_rate = sample_rate
        .checked_mul(2)
        .ok_or_else(|| anyhow::anyhow!("WAV sample rate is too large"))?;
    let mut wav = Vec::with_capacity(44 + pcm16.len());
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&riff_size.to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&byte_rate.to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_size.to_le_bytes());
    wav.extend_from_slice(pcm16);
    Ok(wav)
}

pub fn parse_pcm_rate_from_format(format: &str, fallback: Option<u32>) -> Option<u32> {
    static RATE: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"(?i)(?:^|[;,\s])rate\s*=\s*([0-9]+)(?:$|[;,\s])")
            .expect("constant rate regex")
    });
    RATE.captures(format)
        .and_then(|captures| captures[1].parse::<u32>().ok())
        .filter(|rate| *rate > 0)
        .or(fallback)
}

fn check_pcm16(pcm16: &[u8]) -> Result<()> {
    ensure!(
        pcm16.len() % 2 == 0,
        "PCM16 chunk byteLength must be even, got {}",
        pcm16.len()
    );
    Ok(())
}

pub fn pcm16le_peak_abs(pcm16: &[u8]) -> Result<u32> {
    check_pcm16(pcm16)?;
    let mut peak = 0;
    for bytes in pcm16.chunks_exact(2) {
        peak = peak.max(i32::from(i16::from_le_bytes([bytes[0], bytes[1]])).unsigned_abs());
        // Match the source's early saturation, including -32768 when seen first.
        if peak >= 32767 {
            break;
        }
    }
    Ok(peak)
}

pub fn pcm16le_to_float32(pcm16: &[u8], gain: f64) -> Result<Vec<f32>> {
    check_pcm16(pcm16)?;
    Ok(pcm16
        .chunks_exact(2)
        .map(|bytes| {
            let sample = f64::from(i16::from_le_bytes([bytes[0], bytes[1]])) / 32768.0 * gain;
            sample.clamp(-1.0, 1.0) as f32
        })
        .collect())
}

fn sample_to_i16(sample: f64) -> i16 {
    // JS Math.round rounds half ties toward +infinity, unlike Rust's round().
    (sample.clamp(-1.0, 1.0) * 32767.0 + 0.5).floor() as i16
}

pub fn float32_to_pcm16le(samples: &[f32]) -> Vec<u8> {
    let mut pcm = Vec::with_capacity(samples.len() * 2);
    for sample in samples {
        pcm.extend_from_slice(&sample_to_i16(f64::from(*sample)).to_le_bytes());
    }
    pcm
}

pub struct Pcm16MonoResampler {
    step: f64,
    pos: f64,
    carry_sample: Option<i16>,
}

impl Pcm16MonoResampler {
    pub fn new(input_rate: u32, output_rate: u32) -> Result<Self> {
        ensure!(
            input_rate > 0 && output_rate > 0,
            "PCM sample rates must be positive"
        );
        Ok(Self {
            step: f64::from(input_rate) / f64::from(output_rate),
            pos: 0.0,
            carry_sample: None,
        })
    }

    pub fn reset(&mut self) {
        self.pos = 0.0;
        self.carry_sample = None;
    }

    pub fn process_chunk(&mut self, pcm16: &[u8]) -> Result<Vec<u8>> {
        check_pcm16(pcm16)?;
        if pcm16.is_empty() {
            return Ok(Vec::new());
        }
        let carry = usize::from(self.carry_sample.is_some());
        let count = pcm16.len() / 2 + carry;
        let last = i16::from_le_bytes([pcm16[pcm16.len() - 2], pcm16[pcm16.len() - 1]]);
        if count < 2 {
            self.carry_sample = Some(last);
            return Ok(Vec::new());
        }
        let max_pos = (count - 1) as f64;
        let expected = ((max_pos - self.pos).max(0.0) / self.step).ceil() as usize;
        let mut output = Vec::with_capacity(expected.saturating_mul(2));
        let sample_at = |index: usize| -> f64 {
            let raw = if carry == 1 && index == 0 {
                self.carry_sample.expect("carry present")
            } else {
                let offset = (index - carry) * 2;
                i16::from_le_bytes([pcm16[offset], pcm16[offset + 1]])
            };
            // Every normalized i16 is exactly representable in float32.
            f64::from(raw) / 32768.0
        };
        while self.pos < max_pos {
            let index = self.pos.floor() as usize;
            let fraction = self.pos - index as f64;
            let s0 = sample_at(index);
            let sample = s0 + (sample_at(index + 1) - s0) * fraction;
            output.extend_from_slice(&sample_to_i16(sample).to_le_bytes());
            self.pos += self.step;
        }
        self.carry_sample = Some(last);
        self.pos = (self.pos - max_pos).max(0.0);
        Ok(output)
    }

    /// Finish without inventing a sample beyond the source stream's last point.
    pub fn flush(&mut self) -> Vec<u8> {
        self.reset();
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_fixture_header_and_borrowed_payload() {
        let pcm = [0, 128, 0, 0, 255, 127];
        let wav = pcm16_mono_wav(&pcm, 24000).unwrap();
        assert_eq!(&wav[..12], b"RIFF*\0\0\0WAVE");
        assert_eq!(&wav[24..32], &[192, 93, 0, 0, 128, 187, 0, 0]);
        let parsed = parse_pcm16_mono_wav(&wav).unwrap();
        assert_eq!(parsed.sample_rate, 24000);
        assert_eq!(parsed.pcm16, pcm);
        assert_eq!(parsed.pcm16.as_ptr(), wav[44..].as_ptr());
    }

    #[test]
    fn wav_odd_unknown_chunk_and_malformed_payload() {
        let wav = pcm16_mono_wav(&[1, 0], 16000).unwrap();
        let mut padded = wav[..12].to_vec();
        padded.extend_from_slice(b"JUNK\x01\0\0\0x\0");
        padded.extend_from_slice(&wav[12..]);
        assert_eq!(parse_pcm16_mono_wav(&padded).unwrap().pcm16, &[1, 0]);
        padded.pop();
        assert!(parse_pcm16_mono_wav(&padded).is_err());
    }

    #[test]
    fn pcm_conversion_matches_js_rounding_and_clipping() {
        assert_eq!(
            float32_to_pcm16le(&[-1.0, -0.5, 0.0, 0.5, 1.0]),
            [1, 128, 1, 192, 0, 0, 0, 64, 255, 127]
        );
        assert_eq!(
            pcm16le_to_float32(&[0, 128, 255, 127], 2.0).unwrap(),
            [-1.0, 1.0]
        );
        assert_eq!(pcm16le_peak_abs(&[0, 128]).unwrap(), 32768);
        assert!(pcm16le_to_float32(&[1], 1.0).is_err());
    }

    #[test]
    fn resampler_preserves_chunk_boundary_phase() {
        let pcm = [0, 0, 0, 64, 0, 0, 0, 192, 0, 0];
        let expected = vec![0, 0, 0, 32, 0, 64, 0, 32, 0, 0, 0, 224, 1, 192, 0, 224];
        let mut whole = Pcm16MonoResampler::new(16000, 32000).unwrap();
        assert_eq!(whole.process_chunk(&pcm).unwrap(), expected);
        for size in [2, 4, 6] {
            let mut streaming = Pcm16MonoResampler::new(16000, 32000).unwrap();
            let mut actual = Vec::new();
            for chunk in pcm.chunks(size) {
                actual.extend(streaming.process_chunk(chunk).unwrap());
            }
            actual.extend(streaming.flush());
            assert_eq!(actual, expected);
        }
    }

    #[test]
    fn rate_boundaries_match_source() {
        assert_eq!(
            parse_pcm_rate_from_format("audio/pcm; RATE = 24000;bits=16", None),
            Some(24000)
        );
        assert_eq!(
            parse_pcm_rate_from_format("audio/pcm;bitrate=24000", Some(16000)),
            Some(16000)
        );
        assert_eq!(parse_pcm_rate_from_format("rate=0", None), None);
    }
}
