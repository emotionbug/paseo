use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{Context, Result, anyhow, bail};
use reqwest::{
    Client, Method, Response, StatusCode,
    header::{ACCEPT, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue},
};
use serde_json::Value;
use tokio::sync::watch;
use url::Url;

use super::{MAX_MESSAGE, SERVER_TIMEOUT, State, schema, sse};

pub(super) struct HttpTransport {
    client: Client,
    url: Url,
    headers: HeaderMap,
    protocol: Mutex<Option<HeaderValue>>,
    session: Mutex<Option<HeaderValue>>,
    endpoint: watch::Sender<Option<Url>>,
    pub legacy: bool,
}

#[derive(Default)]
struct Cursor {
    id: Option<String>,
    retry_ms: Option<u64>,
}

impl HttpTransport {
    pub fn new(config: &Value, legacy: bool) -> Result<Self> {
        let url = Url::parse(schema::string(config, "url")?)
            .map_err(|_| anyhow!("MCP URL is invalid"))?;
        if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
            bail!("MCP URL must use HTTP or HTTPS");
        }
        if !url.username().is_empty() || url.password().is_some() {
            bail!("MCP URL credentials are not supported; use configured headers")
        }
        let mut headers = HeaderMap::new();
        if let Some(configured) = config.get("headers") {
            for (key, value) in configured
                .as_object()
                .context("MCP headers must be an object")?
            {
                let key =
                    HeaderName::from_bytes(key.as_bytes()).context("MCP header name is invalid")?;
                let mut value = HeaderValue::from_str(
                    value
                        .as_str()
                        .context("MCP header values must be strings")?,
                )
                .map_err(|_| anyhow!("MCP header value is invalid"))?;
                value.set_sensitive(true);
                headers.insert(key, value);
            }
        }
        // The source SDK follows same-origin redirects only. Keep arbitrary
        // configured bearer/API-key headers away from redirect targets.
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                if attempt.previous().len() >= 10 {
                    return attempt.error("MCP redirect limit exceeded");
                }
                if attempt
                    .previous()
                    .first()
                    .is_some_and(|original| original.origin() == attempt.url().origin())
                {
                    attempt.follow()
                } else {
                    attempt.stop()
                }
            }))
            .connect_timeout(SERVER_TIMEOUT)
            .build()
            .context("creating MCP HTTP client")?;
        let (endpoint, _) = watch::channel(None);
        Ok(Self {
            client,
            url,
            headers,
            protocol: Mutex::new(None),
            session: Mutex::new(None),
            endpoint,
            legacy,
        })
    }

    pub fn set_protocol(&self, version: &str) -> Result<()> {
        *self.protocol.lock().unwrap_or_else(|e| e.into_inner()) =
            Some(HeaderValue::from_str(version)?);
        Ok(())
    }

    pub async fn wait_endpoint(&self) -> Result<()> {
        let mut endpoint = self.endpoint.subscribe();
        loop {
            if endpoint.borrow_and_update().is_some() {
                return Ok(());
            }
            endpoint
                .changed()
                .await
                .context("MCP SSE endpoint channel closed")?;
        }
    }

    fn headers(&self) -> HeaderMap {
        let mut headers = self.headers.clone();
        if !headers.contains_key("mcp-protocol-version") {
            if let Some(version) = self
                .protocol
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
            {
                headers.insert("mcp-protocol-version", version);
            }
        }
        if !self.legacy && !headers.contains_key("mcp-session-id") {
            if let Some(session) = self
                .session
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
            {
                headers.insert("mcp-session-id", session);
            }
        }
        headers
    }

    pub async fn send(&self, message: &Value, state: &Arc<State>) -> Result<()> {
        let endpoint = if self.legacy {
            self.endpoint
                .borrow()
                .clone()
                .context("MCP SSE server has not supplied a POST endpoint")?
        } else {
            self.url.clone()
        };
        let mut headers = self.headers();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        if !self.legacy {
            headers.insert(
                ACCEPT,
                HeaderValue::from_static("application/json, text/event-stream"),
            );
        }
        let response = self
            .client
            .post(endpoint)
            .headers(headers)
            .json(message)
            .send()
            .await
            .map_err(|e| e.without_url())
            .context("MCP HTTP POST failed; request not replayed")?;
        if !response.status().is_success() {
            if !self.legacy && response.status() == StatusCode::NOT_FOUND {
                state.fail("MCP HTTP session expired; requests are not replayed".into());
            }
            bail!(
                "MCP HTTP POST returned status {}; request not replayed",
                response.status().as_u16()
            );
        }
        if !self.legacy {
            if let Some(session) = response.headers().get("mcp-session-id") {
                if session.as_bytes().is_empty()
                    || session.as_bytes().len() > 4096
                    || !session
                        .as_bytes()
                        .iter()
                        .all(|byte| (0x21..=0x7e).contains(byte))
                {
                    bail!("MCP HTTP server returned an invalid session ID");
                }
                let mut session = session.clone();
                session.set_sensitive(true);
                *self.session.lock().unwrap_or_else(|e| e.into_inner()) = Some(session);
            }
        }
        let request_id = message
            .get("method")
            .and_then(|_| message.get("id"))
            .and_then(Value::as_u64);
        // Legacy POST responses are acknowledgements; replies arrive on GET.
        // Notifications and server-request replies likewise have no reply body.
        if self.legacy || request_id.is_none() || response.status() == StatusCode::ACCEPTED {
            return Ok(());
        }
        match media_type(&response) {
            kind if kind.eq_ignore_ascii_case("application/json") => {
                state.receive(read_json(response).await?)
            }
            kind if kind.eq_ignore_ascii_case("text/event-stream") => {
                let mut cursor = Cursor::default();
                let request_id = request_id.expect("checked request id");
                let first = self
                    .read_stream(response, state, &mut cursor, Some(request_id))
                    .await;
                if !state.has_pending(request_id) {
                    return first;
                }
                // A dropped POST stream is resumable only after the server gave
                // an event ID. Resume with GET, never resubmit tools/call.
                if cursor.id.is_none() {
                    return first.and_then(|_| {
                        Err(anyhow!(
                            "MCP response stream ended before the response; request not replayed"
                        ))
                    });
                }
                for attempt in 0..2 {
                    tokio::time::sleep(retry_delay(&cursor, attempt)).await;
                    let resumed = self.open_stream(&cursor, state).await;
                    if let Ok(Some(response)) = resumed {
                        let result = self
                            .read_stream(response, state, &mut cursor, Some(request_id))
                            .await;
                        if !state.has_pending(request_id) {
                            return result;
                        }
                    }
                }
                bail!("MCP response stream could not resume; tool request was not replayed")
            }
            _ => bail!("MCP HTTP request response must be application/json or text/event-stream"),
        }
    }

    async fn open_stream(&self, cursor: &Cursor, state: &Arc<State>) -> Result<Option<Response>> {
        let mut headers = self.headers();
        headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));
        if let Some(id) = cursor.id.as_deref().filter(|id| !id.is_empty()) {
            let mut value = HeaderValue::from_str(id)
                .map_err(|_| anyhow!("MCP SSE event ID cannot be sent as a header"))?;
            value.set_sensitive(true);
            headers.insert("last-event-id", value);
        }
        let response = tokio::time::timeout(
            SERVER_TIMEOUT,
            self.client.get(self.url.clone()).headers(headers).send(),
        )
        .await
        .context("MCP SSE connection timed out")?
        .map_err(|e| e.without_url())
        .context("opening MCP SSE stream")?;
        if !self.legacy && response.status() == StatusCode::METHOD_NOT_ALLOWED {
            return Ok(None);
        }
        if !self.legacy && response.status() == StatusCode::NOT_FOUND {
            state.fail("MCP HTTP session expired; requests are not replayed".into());
        }
        if !response.status().is_success() {
            bail!("MCP SSE GET returned status {}", response.status().as_u16())
        }
        if !media_type(&response).eq_ignore_ascii_case("text/event-stream") {
            bail!("MCP SSE GET must return text/event-stream")
        }
        Ok(Some(response))
    }

    async fn read_stream(
        &self,
        mut response: Response,
        state: &Arc<State>,
        cursor: &mut Cursor,
        request_id: Option<u64>,
    ) -> Result<()> {
        let mut parser = sse::Parser::default();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|e| e.without_url())
            .context("reading MCP SSE stream")?
        {
            for byte in chunk {
                let Some(event) = parser.push(byte)? else {
                    continue;
                };
                if let Some(id) = event.id {
                    cursor.id = if id.is_empty() { None } else { Some(id) };
                }
                if let Some(retry) = event.retry_ms {
                    cursor.retry_ms = Some(retry);
                }
                if event.kind == "endpoint" && self.legacy {
                    let endpoint = self
                        .url
                        .join(&event.data)
                        .map_err(|_| anyhow!("MCP SSE endpoint URL is invalid"))?;
                    if endpoint.origin() != self.url.origin()
                        || !endpoint.username().is_empty()
                        || endpoint.password().is_some()
                    {
                        bail!("MCP SSE endpoint must have the configured server origin");
                    }
                    self.endpoint.send_replace(Some(endpoint));
                } else if !event.data.is_empty()
                    && (event.kind.is_empty() || event.kind == "message")
                {
                    let message = serde_json::from_str(&event.data)
                        .context("invalid MCP SSE JSON message")?;
                    state.receive(message)?;
                    if request_id.is_some_and(|id| !state.has_pending(id)) {
                        return Ok(());
                    }
                }
            }
        }
        Ok(())
    }

    pub async fn listen(&self, state: &Arc<State>) {
        let mut cursor = Cursor::default();
        let mut failures = 0usize;
        loop {
            let open = tokio::select! {
                biased;
                _ = state.stop.cancelled() => return,
                result = self.open_stream(&cursor, state) => result,
            };
            match open {
                Ok(None) => return, // Optional Streamable HTTP GET is unsupported.
                Ok(Some(response)) => {
                    failures = 0;
                    let result = tokio::select! {
                        biased;
                        _ = state.stop.cancelled() => return,
                        result = self.read_stream(response, state, &mut cursor, None) => result,
                    };
                    if let Err(error) = result {
                        if error.downcast_ref::<reqwest::Error>().is_none() {
                            state.fail(error.to_string());
                            return;
                        }
                        failures += 1;
                    }
                }
                Err(error) => {
                    if self.legacy
                        && (self.endpoint.borrow().is_none()
                            || error.downcast_ref::<reqwest::Error>().is_none())
                    {
                        state.fail(error.to_string());
                        return;
                    }
                    failures += 1;
                }
            }
            if !self.legacy && failures >= 2 {
                // POST request streams remain usable when the optional idle GET
                // stream is unavailable. No initialized session is recreated.
                return;
            }
            let delay = if self.legacy {
                timer_delay(cursor.retry_ms.unwrap_or(3000))
            } else {
                retry_delay(&cursor, failures.saturating_sub(1))
            };
            tokio::select! {
                biased;
                _ = state.stop.cancelled() => return,
                _ = tokio::time::sleep(delay) => {}
            }
        }
    }

    pub async fn close(&self) -> Result<()> {
        if self.legacy
            || self
                .session
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_none()
        {
            return Ok(());
        }
        let request = self
            .client
            .request(Method::DELETE, self.url.clone())
            .headers(self.headers());
        let response = tokio::time::timeout(SERVER_TIMEOUT, request.send())
            .await
            .context("MCP session DELETE timed out")?
            .map_err(|e| e.without_url())
            .context("terminating MCP HTTP session")?;
        // A 404 session is already gone; 405 explicitly declines deletion.
        if !response.status().is_success()
            && !matches!(
                response.status(),
                StatusCode::METHOD_NOT_ALLOWED | StatusCode::NOT_FOUND
            )
        {
            bail!(
                "MCP session DELETE returned status {}",
                response.status().as_u16()
            );
        }
        self.session
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        Ok(())
    }
}

fn retry_delay(cursor: &Cursor, attempt: usize) -> Duration {
    timer_delay(
        cursor
            .retry_ms
            .unwrap_or(if attempt == 0 { 1000 } else { 1500 }),
    )
}

fn timer_delay(milliseconds: u64) -> Duration {
    // JS timers coerce an out-of-range delay to 1ms. Preserve that behavior
    // without allowing an untrusted retry field to overflow Tokio's Instant.
    Duration::from_millis(if milliseconds > i32::MAX as u64 {
        1
    } else {
        milliseconds.max(1)
    })
}

fn media_type(response: &Response) -> &str {
    response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|header| header.to_str().ok())
        .and_then(|header| header.split(';').next())
        .map(str::trim)
        .unwrap_or("")
}

async fn read_json(mut response: Response) -> Result<Value> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_MESSAGE as u64)
    {
        bail!("MCP HTTP JSON body exceeds 16 MiB");
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| e.without_url())
        .context("reading MCP HTTP JSON body")?
    {
        if body.len().saturating_add(chunk.len()) > MAX_MESSAGE {
            bail!("MCP HTTP JSON body exceeds 16 MiB")
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body).context("invalid MCP HTTP JSON response")
}
