//! One WebSocket message is one protected record. Kind, FIRST/FINAL and total
//! length are authenticated inside every Noise chunk. No reassembly crosses
//! WebSocket boundaries; a failed record must close its enclosing channel.

use zeroize::{Zeroize, Zeroizing};

use super::{
    CryptoError, FrameKind, MAX_WIRE_BYTES,
    noise::{CipherState, MAX_MESSAGE_BYTES, TAG_BYTES},
};

const HEADER_BYTES: usize = 6;
const LENGTH_BYTES: usize = 2;
const FIRST: u8 = 1;
const FINAL: u8 = 2;
pub const MAX_CHUNK_DATA_BYTES: usize = MAX_MESSAGE_BYTES - TAG_BYTES - HEADER_BYTES;
pub const MAX_FRAME_BYTES: u64 = u32::MAX as u64;
const CHUNK_OVERHEAD_BYTES: usize = LENGTH_BYTES + HEADER_BYTES + TAG_BYTES;

pub fn chunk_count(frame_bytes: usize) -> usize {
    frame_bytes.div_ceil(MAX_CHUNK_DATA_BYTES).max(1)
}

pub fn protected_record_byte_length(frame_bytes: usize) -> Result<usize, CryptoError> {
    if frame_bytes as u64 > MAX_FRAME_BYTES {
        return Err(CryptoError::InvalidLength);
    }
    chunk_count(frame_bytes)
        .checked_mul(CHUNK_OVERHEAD_BYTES)
        .and_then(|overhead| frame_bytes.checked_add(overhead))
        .ok_or(CryptoError::InvalidLength)
}

pub fn encode_protected_record(
    kind: FrameKind,
    frame: &[u8],
    sender: &mut CipherState,
) -> Result<Vec<u8>, CryptoError> {
    let wire_length = protected_record_byte_length(frame.len())?;
    if wire_length > MAX_WIRE_BYTES {
        return Err(CryptoError::WireLimit);
    }
    let chunks = chunk_count(frame.len());
    // Check the entire operation's counter capacity before allocating or using
    // any nonce. The caller still must close on an encryption failure.
    if chunks as u64 > u64::MAX - sender.nonce() {
        return Err(CryptoError::NonceExhausted);
    }
    let mut output = Zeroizing::new(vec![0; wire_length]);
    let mut offset = 0;
    for index in 0..chunks {
        let data_start = index * MAX_CHUNK_DATA_BYTES;
        let data_end = frame.len().min(data_start + MAX_CHUNK_DATA_BYTES);
        let data = &frame[data_start..data_end];
        let ciphertext_length = HEADER_BYTES + data.len() + TAG_BYTES;
        output[offset..offset + LENGTH_BYTES]
            .copy_from_slice(&(ciphertext_length as u16).to_be_bytes());
        let start = offset + LENGTH_BYTES;
        let plaintext_end = start + HEADER_BYTES + data.len();
        output[start] = kind as u8;
        output[start + 1] =
            if index == 0 { FIRST } else { 0 } | if index + 1 == chunks { FINAL } else { 0 };
        output[start + 2..start + HEADER_BYTES]
            .copy_from_slice(&(frame.len() as u32).to_be_bytes());
        output[start + HEADER_BYTES..plaintext_end].copy_from_slice(data);
        let tag = sender.encrypt_detached(&[], &mut output[start..plaintext_end])?;
        output[plaintext_end..plaintext_end + TAG_BYTES].copy_from_slice(&tag);
        offset = plaintext_end + TAG_BYTES;
    }
    Ok(std::mem::take(&mut *output))
}

#[derive(Debug, PartialEq, Eq)]
pub struct DecodedFrame {
    pub kind: FrameKind,
    pub data: Vec<u8>,
}

/// Takes ownership so decryption and compaction reuse the bounded ciphertext
/// allocation. No allocation uses an authenticated *claimed* frame length, and
/// no plaintext is returned until all chunks and exact record size are valid.
/// Nonces advance once per successfully authenticated chunk, even if its framing
/// then fails; the surrounding channel must close instead of retrying a record.
pub fn decode_protected_record(
    record: Vec<u8>,
    receiver: &mut CipherState,
) -> Result<DecodedFrame, CryptoError> {
    if record.len() > MAX_WIRE_BYTES {
        return Err(CryptoError::WireLimit);
    }
    if record.is_empty() {
        return Err(CryptoError::Record("empty record"));
    }
    let mut record = Zeroizing::new(record);
    let mut offset = 0;
    let mut index = 0;
    let mut written = 0;
    let mut assembly: Option<(FrameKind, usize, usize)> = None;
    while offset < record.len() {
        if let Some((_, _, expected_chunks)) = assembly {
            if index >= expected_chunks {
                return Err(CryptoError::Record("unexpected chunk after final"));
            }
        }
        if record.len() - offset < LENGTH_BYTES {
            return Err(CryptoError::Record("record truncated"));
        }
        let length = u16::from_be_bytes([record[offset], record[offset + 1]]) as usize;
        if !(HEADER_BYTES + TAG_BYTES..=MAX_MESSAGE_BYTES).contains(&length) {
            return Err(CryptoError::Record("invalid chunk length"));
        }
        let start = offset + LENGTH_BYTES;
        if record.len() - start < length {
            return Err(CryptoError::Record("record truncated"));
        }
        let end = start + length;
        let plaintext_length = receiver.decrypt_in_place(&[], &mut record[start..end])?;
        let kind = match record[start] {
            0 => FrameKind::Text,
            1 => FrameKind::Binary,
            _ => return Err(CryptoError::Record("invalid frame kind")),
        };
        let flags = record[start + 1];
        if flags & !(FIRST | FINAL) != 0 {
            return Err(CryptoError::Record("reserved flag set"));
        }
        let total = u32::from_be_bytes([
            record[start + 2],
            record[start + 3],
            record[start + 4],
            record[start + 5],
        ]) as usize;
        let expected_chunks = match assembly {
            None => {
                if flags & FIRST == 0 {
                    return Err(CryptoError::Record("record does not start with FIRST"));
                }
                if protected_record_byte_length(total)? != record.len() {
                    return Err(CryptoError::Record("record length mismatch"));
                }
                let count = chunk_count(total);
                assembly = Some((kind, total, count));
                count
            }
            Some((expected_kind, expected_total, count)) => {
                if flags & FIRST != 0 {
                    return Err(CryptoError::Record("FIRST repeated inside record"));
                }
                if kind != expected_kind {
                    return Err(CryptoError::Record("frame kind changed inside record"));
                }
                if total != expected_total {
                    return Err(CryptoError::Record("frame length changed inside record"));
                }
                count
            }
        };
        let last = index + 1 == expected_chunks;
        if (flags & FINAL != 0) != last {
            return Err(CryptoError::Record("FINAL flag mismatch"));
        }
        let expected_data = if last {
            total - index * MAX_CHUNK_DATA_BYTES
        } else {
            MAX_CHUNK_DATA_BYTES
        };
        let data_length = plaintext_length - HEADER_BYTES;
        if data_length != expected_data {
            return Err(CryptoError::Record("chunk size mismatch"));
        }
        record.copy_within(start + HEADER_BYTES..start + plaintext_length, written);
        written += data_length;
        index += 1;
        offset = end;
    }
    let (kind, total, expected_chunks) = assembly.ok_or(CryptoError::Record("empty record"))?;
    if index != expected_chunks || written != total {
        return Err(CryptoError::Record("record incomplete"));
    }
    record[written..].zeroize();
    record.truncate(written);
    Ok(DecodedFrame {
        kind,
        data: std::mem::take(&mut *record),
    })
}
