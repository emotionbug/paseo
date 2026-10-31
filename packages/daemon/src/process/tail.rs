use std::collections::VecDeque;

pub(super) struct Tail {
    bytes: VecDeque<u8>,
    limit: usize,
    truncated: bool,
}

impl Tail {
    pub(super) fn new(limit: usize) -> Self {
        Self {
            bytes: VecDeque::new(),
            limit,
            truncated: false,
        }
    }

    pub(super) fn append(&mut self, chunk: &[u8]) {
        if chunk.len() >= self.limit {
            self.truncated |= !self.bytes.is_empty() || chunk.len() > self.limit;
            self.bytes.clear();
            self.bytes.extend(&chunk[chunk.len() - self.limit..]);
        } else {
            let overflow = chunk.len().saturating_sub(self.limit - self.bytes.len());
            if overflow != 0 {
                self.truncated = true;
                self.bytes.drain(..overflow);
            }
            self.bytes.extend(chunk);
        }
    }

    pub(super) fn text(&self) -> String {
        let marker: &[u8] = if self.truncated {
            b"[stderr truncated; showing tail]\n"
        } else {
            b""
        };
        let mut bytes = Vec::with_capacity(marker.len() + self.bytes.len());
        bytes.extend_from_slice(marker);
        let (first, second) = self.bytes.as_slices();
        bytes.extend_from_slice(first);
        bytes.extend_from_slice(second);
        String::from_utf8(bytes)
            .unwrap_or_else(|error| String::from_utf8_lossy(error.as_bytes()).into_owned())
    }
}
