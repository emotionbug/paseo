use anyhow::{Context, Result, bail};

use super::MAX_MESSAGE;

#[derive(Default)]
pub(super) struct Event {
    pub kind: String,
    pub data: String,
    pub id: Option<String>,
    pub retry_ms: Option<u64>,
}

#[derive(Default)]
pub(super) struct Parser {
    line: Vec<u8>,
    event: Event,
    skip_lf: bool,
    first_line: bool,
    started: bool,
    event_size: usize,
}

impl Parser {
    pub fn push(&mut self, byte: u8) -> Result<Option<Event>> {
        if !self.started {
            self.started = true;
            self.first_line = true;
        }
        if self.skip_lf {
            self.skip_lf = false;
            if byte == b'\n' {
                return Ok(None);
            }
        }
        match byte {
            b'\r' | b'\n' => {
                self.skip_lf = byte == b'\r';
                self.finish_line()
            }
            byte => {
                if self.line.len() >= MAX_MESSAGE {
                    bail!("MCP SSE line exceeds 16 MiB")
                }
                self.line.push(byte);
                Ok(None)
            }
        }
    }

    fn finish_line(&mut self) -> Result<Option<Event>> {
        if self.line.is_empty() {
            self.first_line = false;
            self.event_size = 0;
            let mut event = std::mem::take(&mut self.event);
            if event.data.ends_with('\n') {
                event.data.pop();
            }
            return Ok(Some(event));
        }
        let line = std::str::from_utf8(&self.line).context("MCP SSE contains invalid UTF-8")?;
        let line = if self.first_line {
            line.strip_prefix('\u{feff}').unwrap_or(line)
        } else {
            line
        };
        self.first_line = false;
        // Comments/keep-alives do not accumulate in the current event.
        if !line.starts_with(':') {
            let (field, value) = line.split_once(':').unwrap_or((line, ""));
            let value = value.strip_prefix(' ').unwrap_or(value);
            match field {
                "data" => {
                    self.event_size = self.event_size.saturating_add(value.len() + 1);
                    if self.event_size > MAX_MESSAGE {
                        bail!("MCP SSE event exceeds 16 MiB")
                    }
                    self.event.data.push_str(value);
                    self.event.data.push('\n');
                }
                "event" => self.event.kind = value.to_owned(),
                "id" if !value.contains('\0') => {
                    if value.len() > 4096 {
                        bail!("MCP SSE event ID exceeds 4096 bytes")
                    }
                    self.event.id = Some(value.to_owned());
                }
                "retry" if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) => {
                    self.event.retry_ms = value.parse().ok();
                }
                _ => {}
            }
        }
        self.line.clear();
        Ok(None)
    }
}
