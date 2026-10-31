use std::{
    borrow::Cow,
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{Result, anyhow, bail, ensure};
use bytes::{BufMut, Bytes, BytesMut};
use serde_json::{Value, json};
use tokio::{
    fs::{self, File, OpenOptions},
    io::AsyncWriteExt,
    sync::{mpsc, oneshot, watch},
    task::JoinSet,
    time::Instant,
};
use tokio_util::sync::CancellationToken;

use crate::transport::{OwnedOperation, RequestContext};

pub const FILE_BEGIN: u8 = 0x10;
pub const FILE_CHUNK: u8 = 0x11;
pub const FILE_END: u8 = 0x12;

/// Begin payloads are UTF-8 JSON, preceded by a big-endian u16 byte length.
pub fn encode_frame(opcode: u8, id: &str, payload: &[u8]) -> Result<Bytes> {
    ensure!(!id.is_empty(), "File transfer requestId is required");
    ensure!(id.len() <= 255, "File transfer requestId is too long");
    ensure!(
        matches!(opcode, FILE_BEGIN | FILE_CHUNK | FILE_END),
        "Invalid file transfer opcode"
    );
    if opcode == FILE_BEGIN {
        ensure!(
            payload.len() <= u16::MAX as usize,
            "FileBegin metadata is too long"
        );
        validate_metadata(payload)?;
    }
    ensure!(
        opcode != FILE_END || payload.is_empty(),
        "FileEnd must not contain a payload"
    );
    let mut frame = BytesMut::with_capacity(
        2 + id.len() + payload.len() + if opcode == FILE_BEGIN { 2 } else { 0 },
    );
    frame.put_u8(opcode);
    frame.put_u8(id.len() as u8);
    frame.extend_from_slice(id.as_bytes());
    if opcode == FILE_BEGIN {
        frame.put_u16(payload.len() as u16);
    }
    frame.extend_from_slice(payload);
    Ok(frame.freeze())
}

fn validate_metadata(payload: &[u8]) -> Result<()> {
    let value: Value = serde_json::from_slice(payload)?;
    ensure!(
        value.is_object()
            && value["mime"].as_str().is_some_and(|s| !s.is_empty())
            && value["size"].as_u64().is_some()
            && matches!(value["encoding"].as_str(), Some("utf-8" | "binary"))
            && value["modifiedAt"].is_string()
            && value.get("revision").is_none_or(Value::is_string)
            && value.get("fileName").is_none_or(Value::is_string),
        "Invalid FileBegin metadata"
    );
    Ok(())
}

struct Frame<'a> {
    opcode: u8,
    id: Cow<'a, str>,
    payload_offset: usize,
}

fn decode_frame(bytes: &[u8]) -> Option<Frame<'_>> {
    if bytes.len() < 2 || !matches!(bytes[0], FILE_BEGIN | FILE_CHUNK | FILE_END) {
        return None;
    }
    let length = bytes[1] as usize;
    if length == 0 || length > bytes.len() - 2 {
        return None;
    }
    let offset = 2 + length;
    let body = &bytes[offset..];
    match bytes[0] {
        FILE_BEGIN => {
            if body.len() < 2 || u16::from_be_bytes([body[0], body[1]]) as usize != body.len() - 2 {
                return None;
            }
            // TextDecoder replaces malformed UTF-8 before JSON parsing in the source protocol.
            validate_metadata(String::from_utf8_lossy(&body[2..]).as_bytes()).ok()?;
        }
        FILE_END if !body.is_empty() => return None,
        _ => {}
    }
    Some(Frame {
        opcode: bytes[0],
        id: String::from_utf8_lossy(&bytes[2..offset]),
        payload_offset: offset,
    })
}

type UploadKey = (String, String);
struct Command {
    opcode: u8,
    payload: Bytes,
    finished: oneshot::Sender<Result<()>>,
}
struct Slot {
    id: String,
    sender: mpsc::Sender<Command>,
    deadline: watch::Sender<Instant>,
    cancel: CancellationToken,
}
#[derive(Default)]
struct Registry {
    uploads: HashMap<UploadKey, Arc<Slot>>,
    tasks: JoinSet<Result<()>>,
}

pub struct Transfers {
    home: PathBuf,
    ttl: Duration,
    registry: Arc<Mutex<Registry>>,
    cancel: CancellationToken,
}

impl Transfers {
    pub fn new(home: PathBuf, ttl: Duration) -> Self {
        Self {
            home,
            ttl,
            registry: Arc::new(Mutex::new(Registry::default())),
            cancel: CancellationToken::new(),
        }
    }

    pub async fn begin(&self, context: &RequestContext, request: &Value) -> Result<()> {
        let request_id = string_field(request, "requestId")?;
        ensure!(
            !request_id.is_empty() && request_id.len() <= 255,
            "Invalid file transfer request ID"
        );
        let file_name = string_field(request, "fileName")?;
        let mime_type = string_field(request, "mimeType")?;
        ensure!(
            !file_name.is_empty() && !mime_type.is_empty(),
            "File name and MIME type are required"
        );
        let size = request["size"]
            .as_u64()
            .ok_or_else(|| anyhow!("Invalid upload size"))?;
        string_field(request, "modifiedAt")?;
        let mut registry = self.registry.lock().unwrap();
        ensure!(!self.cancel.is_cancelled(), "File transfers are shut down");
        while let Some(result) = registry.tasks.try_join_next() {
            result??;
        }
        // Retaining the requesting operation is required: later binary frames carry no request owner.
        context.bind_file_transfer(request_id)?;
        let operation = match context.retain_operation(&["file.upload.response"]) {
            Ok(operation) => operation,
            Err(error) => {
                context.release_file_transfer(request_id);
                return Err(error);
            }
        };
        let key = (context.source_id().to_owned(), request_id.to_owned());
        let id = format!("upload_{}", uuid::Uuid::new_v4());
        let file_name = sanitize_file_name(file_name);
        let directory = self.home.join("uploads").join(&id);
        let upload = Upload {
            request_id: request_id.to_owned(),
            id: id.clone(),
            path: directory.join(&file_name),
            directory,
            file_name,
            mime_type: mime_type.to_owned(),
            size,
            received: 0,
            started: false,
            completed: false,
        };
        let (sender, receiver) = mpsc::channel(8);
        let (deadline, deadlines) = watch::channel(Instant::now() + self.ttl);
        let slot = Arc::new(Slot {
            id,
            sender,
            deadline,
            cancel: self.cancel.child_token(),
        });
        if let Some(old) = registry.uploads.insert(key.clone(), slot.clone()) {
            old.cancel.cancel();
        }
        registry.tasks.spawn(run_upload(
            self.registry.clone(),
            key,
            slot,
            context.clone(),
            operation,
            upload,
            receiver,
            deadlines,
        ));
        Ok(())
    }

    pub async fn receive(&self, context: &RequestContext, bytes: Bytes) -> Result<bool> {
        let Some(frame) = decode_frame(&bytes) else {
            return Ok(false);
        };
        let key = (context.source_id().to_owned(), frame.id.into_owned());
        let slot = self.registry.lock().unwrap().uploads.get(&key).cloned();
        let Some(slot) = slot else {
            return Ok(false);
        };
        if slot.cancel.is_cancelled() || context.cancellation().is_cancelled() {
            return Ok(false);
        }
        slot.deadline.send_replace(Instant::now() + self.ttl);
        let (finished, completion) = oneshot::channel();
        let command = Command {
            opcode: frame.opcode,
            payload: if frame.opcode == FILE_CHUNK {
                bytes.slice(frame.payload_offset..)
            } else {
                Bytes::new()
            },
            finished,
        };
        if slot.sender.send(command).await.is_err() {
            return Ok(true);
        }
        if let Ok(result) = completion.await {
            result?;
        }
        Ok(true)
    }

    pub async fn shutdown(&self) -> Result<()> {
        let mut tasks = {
            let mut registry = self.registry.lock().unwrap();
            self.cancel.cancel();
            std::mem::take(&mut registry.tasks)
        };
        let mut first_error = None;
        while let Some(result) = tasks.join_next().await {
            let result = result.map_err(anyhow::Error::from).and_then(|r| r);
            if let Err(error) = result {
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
        }
        if let Some(error) = first_error {
            return Err(error);
        }
        Ok(())
    }
}

impl Drop for Transfers {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

struct Upload {
    request_id: String,
    id: String,
    directory: PathBuf,
    path: PathBuf,
    file_name: String,
    mime_type: String,
    size: u64,
    received: u64,
    started: bool,
    completed: bool,
}

impl Upload {
    async fn apply(&mut self, opcode: u8, bytes: &[u8]) -> Result<bool> {
        match opcode {
            FILE_BEGIN => {
                fs::create_dir_all(&self.directory).await?;
                fs::write(&self.path, []).await?;
                self.started = true;
                Ok(false)
            }
            FILE_CHUNK => {
                ensure!(self.started, "Upload chunks arrived before file begin.");
                let next = self.received as u128 + bytes.len() as u128;
                ensure!(
                    next <= self.size as u128,
                    "Upload exceeded declared size: expected {}, received {}.",
                    self.size,
                    next
                );
                let mut file = OpenOptions::new().append(true).open(&self.path).await?;
                file.write_all(bytes).await?;
                file.flush().await?;
                self.received = next as u64;
                Ok(false)
            }
            FILE_END => {
                ensure!(
                    self.received == self.size,
                    "Upload size mismatch: expected {}, received {}.",
                    self.size,
                    self.received
                );
                self.completed = true;
                Ok(true)
            }
            _ => bail!("Invalid file transfer opcode"),
        }
    }
    fn response(&self, error: Option<&str>) -> Value {
        json!({"type":"file.upload.response", "payload":{
            "requestId":self.request_id,
            "file": if error.is_some() { Value::Null } else { json!({"type":"uploaded_file",
                "id":self.id,"fileName":self.file_name,"mimeType":self.mime_type,
                "size":self.size,"path":self.path}) }, "error":error }})
    }
    async fn cleanup(&self) -> Result<()> {
        if !self.completed {
            match fs::remove_dir_all(&self.directory).await {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }
}

async fn run_upload(
    registry: Arc<Mutex<Registry>>,
    key: UploadKey,
    slot: Arc<Slot>,
    context: RequestContext,
    operation: OwnedOperation,
    mut upload: Upload,
    mut commands: mpsc::Receiver<Command>,
    mut deadlines: watch::Receiver<Instant>,
) -> Result<()> {
    let owner_cancel = operation.cancellation();
    let result: Result<()> = async {
        loop {
            let deadline = *deadlines.borrow_and_update();
            tokio::select! { biased;
                _ = slot.cancel.cancelled() => break,
                _ = owner_cancel.cancelled() => break,
                _ = tokio::time::sleep_until(deadline) => {
                    if *deadlines.borrow() <= Instant::now() { break; }
                }
                changed = deadlines.changed() => { if changed.is_err() { break; } }
                command = commands.recv() => {
                    let Some(command) = command else { break; };
                    let applied = upload.apply(command.opcode, &command.payload).await;
                    let response = match applied {
                        Ok(false) => { let _ = command.finished.send(Ok(())); continue; }
                        Ok(true) => upload.response(None),
                        Err(error) => {
                            upload.cleanup().await?;
                            upload.response(Some(&error.to_string()))
                        }
                    };
                    // Cancellation wins over a write that was already in progress.
                    let delivered = if slot.cancel.is_cancelled() || owner_cancel.is_cancelled() {
                        Ok(())
                    } else { operation.emit(response).await };
                    let _ = command.finished.send(delivered);
                    break;
                }
            }
        }
        Ok(())
    }
    .await;
    {
        let mut registry = registry.lock().unwrap();
        if registry
            .uploads
            .get(&key)
            .is_some_and(|current| current.id == slot.id)
        {
            registry.uploads.remove(&key);
            context.release_file_transfer(&key.1);
        }
    }
    commands.close();
    let cleanup = upload.cleanup().await;
    let release = operation.release().await;
    result.and(cleanup).and(release)
}

fn string_field<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("{key} is required"))
}

fn sanitize_file_name(value: &str) -> String {
    // Node's POSIX basename removes trailing slashes before selecting the final component.
    let basename = value
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or_default();
    let replaced: String = basename
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
                '_'
            } else {
                c
            }
        })
        .collect();
    let name = replaced.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}');
    let name = if name.is_empty() || name == "." || name == ".." {
        "upload"
    } else {
        name
    };
    if name.len() <= 255 {
        return name.to_owned();
    }
    let extension = name
        .rfind('.')
        .filter(|index| *index > 0)
        .map(|index| &name[index..])
        .unwrap_or("");
    let extension = if extension.len() < 255 { extension } else { "" };
    let mut end = 255 - extension.len();
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    let mut result = String::with_capacity(end + extension.len());
    result.push_str(&name[..end]);
    result.push_str(extension);
    result
}

struct DownloadEntry {
    path: PathBuf,
    file_name: String,
    mime_type: String,
    expires: Instant,
}

pub struct Downloads {
    ttl: Duration,
    tokens: Mutex<HashMap<String, DownloadEntry>>,
}

pub struct FileDownload {
    pub file: File,
    pub size: u64,
    pub mime_type: String,
    pub file_name: String,
    pub content_disposition: String,
}

impl Downloads {
    pub fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            tokens: Mutex::new(HashMap::new()),
        }
    }

    /// The caller has already resolved workspace containment and file metadata.
    pub async fn issue(
        &self,
        path: PathBuf,
        _relative: String,
        file_name: String,
        mime_type: String,
        _size: u64,
    ) -> Result<String> {
        ensure!(fs::canonicalize(&path).await? == path, "File not found");
        ensure!(fs::metadata(&path).await?.is_file(), "File not found");
        let now = Instant::now();
        let mut tokens = self.tokens.lock().unwrap();
        tokens.retain(|_, entry| entry.expires > now);
        let token = uuid::Uuid::new_v4().to_string();
        tokens.insert(
            token.clone(),
            DownloadEntry {
                path,
                file_name,
                mime_type,
                expires: now + self.ttl,
            },
        );
        Ok(token)
    }

    /// None means an invalid/expired token (HTTP 403); an error means unavailable file (HTTP 404).
    /// Tokens are consumed even when opening their file fails.
    pub async fn open(&self, token: &str) -> Result<Option<FileDownload>> {
        let entry = self.tokens.lock().unwrap().remove(token);
        let Some(entry) = entry.filter(|entry| entry.expires > Instant::now()) else {
            return Ok(None);
        };
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        let file = options.open(&entry.path).await?;
        let metadata = file.metadata().await?;
        ensure!(metadata.is_file(), "File not found");
        ensure!(
            fs::canonicalize(&entry.path).await? == entry.path,
            "File not found"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let current = fs::metadata(&entry.path).await?;
            ensure!(
                metadata.dev() == current.dev() && metadata.ino() == current.ino(),
                "File not found"
            );
        }
        Ok(Some(FileDownload {
            file,
            size: metadata.len(),
            content_disposition: content_disposition(&entry.file_name),
            mime_type: entry.mime_type,
            file_name: entry.file_name,
        }))
    }
}

fn content_disposition(name: &str) -> String {
    let mut result = String::from("attachment; filename=\"");
    let mut non_ascii = false;
    // JavaScript's non-Unicode regex replaces each UTF-16 code unit, including surrogate pairs.
    for unit in name.encode_utf16() {
        if !(0x20..=0x7e).contains(&unit) {
            result.push('?');
            non_ascii = true;
        } else {
            let ch = unit as u8 as char;
            if matches!(ch, '\\' | '"') {
                result.push('\\');
            }
            result.push(ch);
        }
    }
    result.push('"');
    let percent_escape = name
        .as_bytes()
        .windows(3)
        .any(|b| b[0] == b'%' && b[1].is_ascii_hexdigit() && b[2].is_ascii_hexdigit());
    if non_ascii || percent_escape {
        result.push_str("; filename*=UTF-8''");
        const HEX: &[u8] = b"0123456789ABCDEF";
        for byte in name.bytes() {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'!' | b'~') {
                result.push(byte as char);
            } else {
                result.push('%');
                result.push(HEX[(byte >> 4) as usize] as char);
                result.push(HEX[(byte & 15) as usize] as char);
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::OutboundFrame;
    use std::{collections::HashSet, sync::RwLock};

    fn metadata() -> Vec<u8> {
        serde_json::to_vec(
            &json!({"mime":"text/plain","size":3,"encoding":"binary","modifiedAt":""}),
        )
        .unwrap()
    }

    fn request(name: &str, size: u64) -> Value {
        json!({"type":"file.upload.request","requestId":"same","fileName":name,
            "mimeType":"text/plain","size":size,"modifiedAt":""})
    }

    async fn join_tasks(transfers: &Transfers) {
        let mut tasks = std::mem::take(&mut transfers.registry.lock().unwrap().tasks);
        tokio::time::timeout(Duration::from_secs(5), async {
            while let Some(result) = tasks.join_next().await {
                result.unwrap().unwrap();
            }
        })
        .await
        .unwrap();
    }

    #[test]
    fn binary_wire_lengths_and_metadata_validation() {
        let payload = metadata();
        let bytes = encode_frame(FILE_BEGIN, "é", &payload).unwrap();
        assert_eq!(&bytes[..4], &[FILE_BEGIN, 2, 0xc3, 0xa9]);
        assert_eq!(&bytes[4..6], &(payload.len() as u16).to_be_bytes());
        let decoded = decode_frame(&bytes).unwrap();
        assert_eq!(decoded.id, "é");
        assert_eq!(decoded.opcode, FILE_BEGIN);
        assert!(encode_frame(FILE_CHUNK, "", b"").is_err());
        assert!(encode_frame(FILE_CHUNK, &"a".repeat(256), b"").is_err());
        assert!(encode_frame(FILE_BEGIN, "a", &vec![b' '; 65536]).is_err());
        assert!(
            encode_frame(
                FILE_BEGIN,
                "a",
                br#"{"mime":"","size":0,"encoding":"binary","modifiedAt":""}"#
            )
            .is_err()
        );
        assert!(encode_frame(FILE_END, "a", b"x").is_err());
        for bytes in [
            vec![],
            vec![FILE_BEGIN],
            vec![FILE_END, 0],
            vec![FILE_END, 1, b'a', 1],
            vec![FILE_BEGIN, 1, b'a', 0, 1],
            vec![0x99, 1, b'a'],
        ] {
            assert!(decode_frame(&bytes).is_none());
        }
        let mut trailing = encode_frame(FILE_BEGIN, "a", &payload).unwrap().to_vec();
        trailing.push(0);
        assert!(decode_frame(&trailing).is_none());
        assert_eq!(
            encode_frame(FILE_CHUNK, "a", b"xyz").unwrap().as_ref(),
            &[FILE_CHUNK, 1, b'a', b'x', b'y', b'z']
        );
        assert!(decode_frame(&[FILE_END, 1, 0xff]).is_some());
    }

    #[test]
    fn names_preserve_unicode_and_extensions() {
        assert_eq!(sanitize_file_name("../../etc/passwd"), "passwd");
        assert_eq!(sanitize_file_name("dir\\name.txt"), "dir_name.txt");
        assert_eq!(sanitize_file_name(" a<b>:\"c|?*.txt "), "a_b___c___.txt");
        assert_eq!(sanitize_file_name("line\nbreak.txt"), "line_break.txt");
        assert_eq!(sanitize_file_name(".."), "upload");
        assert_eq!(
            sanitize_file_name("résumé [final] & notes, v2.pdf"),
            "résumé [final] & notes, v2.pdf"
        );
        assert_eq!(
            sanitize_file_name(&format!("{}.xlsx", "绩".repeat(100))),
            format!("{}.xlsx", "绩".repeat(83))
        );
    }

    #[test]
    fn disposition_matches_utf16_fallback_and_rfc5987() {
        assert_eq!(
            content_disposition("notes.txt"),
            "attachment; filename=\"notes.txt\""
        );
        assert_eq!(
            content_disposition("a\"b\\c"),
            "attachment; filename=\"a\\\"b\\\\c\""
        );
        assert_eq!(
            content_disposition("😀.txt"),
            "attachment; filename=\"??.txt\"; filename*=UTF-8''%F0%9F%98%80.txt"
        );
        assert_eq!(
            content_disposition("x%20(').txt"),
            "attachment; filename=\"x%20(').txt\"; filename*=UTF-8''x%2520%28%27%29.txt"
        );
        assert_eq!(
            content_disposition("a\r\n.txt"),
            "attachment; filename=\"a??.txt\"; filename*=UTF-8''a%0D%0A.txt"
        );
    }

    #[tokio::test]
    async fn uploads_are_source_bound_and_keep_completed_files() {
        let home = tempfile::tempdir().unwrap();
        let transfers = Transfers::new(home.path().to_owned(), Duration::from_secs(60));
        let permissions = Arc::new(RwLock::new(HashSet::from([
            "workspace.read".to_owned(),
            "workspace.write".to_owned(),
        ])));
        let (a, mut outbound) = RequestContext::attach(
            "a".into(),
            "session".into(),
            "client".into(),
            json!({"owned_subscriptions":true}),
            json!({}),
            permissions.clone(),
            false,
            None,
            CancellationToken::new(),
        );
        let (b, _other_outbound) = RequestContext::attach(
            "b".into(),
            "session".into(),
            "client".into(),
            json!({"owned_subscriptions":true}),
            json!({}),
            permissions,
            false,
            None,
            CancellationToken::new(),
        );
        let message = request("notes.txt", 3);
        let requesting = a.for_request(message.clone());
        transfers.begin(&requesting, &message).await.unwrap();
        requesting.finish_request();
        assert!(
            !transfers
                .receive(&b, encode_frame(FILE_CHUNK, "same", b"bad").unwrap())
                .await
                .unwrap()
        );
        transfers
            .receive(&a, encode_frame(FILE_BEGIN, "same", &metadata()).unwrap())
            .await
            .unwrap();
        transfers
            .receive(&a, encode_frame(FILE_CHUNK, "same", b"abc").unwrap())
            .await
            .unwrap();
        transfers
            .receive(&a, encode_frame(FILE_END, "same", b"").unwrap())
            .await
            .unwrap();
        let frame = outbound.recv().await.unwrap();
        let OutboundFrame::Text(text) = &frame.frame else {
            panic!("Expected upload response");
        };
        let response: Value = serde_json::from_str(text).unwrap();
        let file = &response["message"]["payload"]["file"];
        let path = PathBuf::from(file["path"].as_str().unwrap());
        assert_eq!(fs::read(&path).await.unwrap(), b"abc");
        assert_eq!(
            response["message"],
            json!({"type":"file.upload.response","payload":{
            "requestId":"same","file":{"type":"uploaded_file","id":file["id"],"fileName":"notes.txt",
            "mimeType":"text/plain","size":3,"path":path},"error":null}})
        );
        transfers.shutdown().await.unwrap();
        assert!(path.exists());
    }

    #[tokio::test]
    async fn partial_uploads_clean_up_on_timeout_detach_and_replacement() {
        let home = tempfile::tempdir().unwrap();
        let transfers = Transfers::new(home.path().to_owned(), Duration::from_secs(60));
        let source_cancel = CancellationToken::new();
        let (context, _outbound) = RequestContext::attach(
            "source".into(),
            "session".into(),
            "client".into(),
            json!({}),
            json!({}),
            Arc::new(RwLock::new(HashSet::from(["workspace.write".to_owned()]))),
            false,
            None,
            source_cancel.clone(),
        );
        let message = request("old.txt", 3);
        let requesting = context.for_request(message.clone());
        transfers.begin(&requesting, &message).await.unwrap();
        transfers
            .receive(
                &context,
                encode_frame(FILE_BEGIN, "same", &metadata()).unwrap(),
            )
            .await
            .unwrap();
        let key = ("source".to_owned(), "same".to_owned());
        let old = transfers.registry.lock().unwrap().uploads[&key].clone();
        let old_directory = home.path().join("uploads").join(&old.id);
        let message = request("new.txt", 3);
        transfers
            .begin(&context.for_request(message.clone()), &message)
            .await
            .unwrap();
        old.deadline.send_replace(Instant::now());
        transfers
            .receive(
                &context,
                encode_frame(FILE_BEGIN, "same", &metadata()).unwrap(),
            )
            .await
            .unwrap();
        let replacement = transfers.registry.lock().unwrap().uploads[&key].clone();
        assert_ne!(replacement.id, old.id);
        assert!(
            home.path()
                .join("uploads")
                .join(&replacement.id)
                .join("new.txt")
                .exists()
        );
        replacement.deadline.send_replace(Instant::now());
        join_tasks(&transfers).await;
        assert!(!old_directory.exists());
        assert!(!home.path().join("uploads").join(&replacement.id).exists());
        assert!(transfers.registry.lock().unwrap().uploads.is_empty());
        transfers
            .begin(&context.for_request(message.clone()), &message)
            .await
            .unwrap();
        transfers
            .receive(
                &context,
                encode_frame(FILE_BEGIN, "same", &metadata()).unwrap(),
            )
            .await
            .unwrap();
        let detached = transfers.registry.lock().unwrap().uploads[&key].id.clone();
        source_cancel.cancel();
        join_tasks(&transfers).await;
        assert!(!home.path().join("uploads").join(detached).exists());
        transfers.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn chunk_errors_have_exact_messages_and_remove_partial_data() {
        let home = tempfile::tempdir().unwrap();
        let directory = home.path().join("upload");
        let mut upload = Upload {
            request_id: "r".into(),
            id: "upload".into(),
            path: directory.join("file.txt"),
            directory,
            file_name: "file.txt".into(),
            mime_type: "text/plain".into(),
            size: 3,
            received: 0,
            started: false,
            completed: false,
        };
        assert_eq!(
            upload
                .apply(FILE_CHUNK, b"x")
                .await
                .unwrap_err()
                .to_string(),
            "Upload chunks arrived before file begin."
        );
        upload.apply(FILE_BEGIN, b"").await.unwrap();
        upload.apply(FILE_CHUNK, b"ab").await.unwrap();
        assert_eq!(
            upload.apply(FILE_END, b"").await.unwrap_err().to_string(),
            "Upload size mismatch: expected 3, received 2."
        );
        let error = upload
            .apply(FILE_CHUNK, b"cd")
            .await
            .unwrap_err()
            .to_string();
        assert_eq!(
            error,
            "Upload exceeded declared size: expected 3, received 4."
        );
        assert_eq!(
            upload.response(Some(&error)),
            json!({"type":"file.upload.response",
            "payload":{"requestId":"r","file":null,"error":error}})
        );
        upload.cleanup().await.unwrap();
        assert!(!upload.directory.exists());
    }

    #[tokio::test]
    async fn download_tokens_are_single_use_expire_and_read_current_size() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("report.txt");
        fs::write(&path, b"old").await.unwrap();
        let downloads = Downloads::new(Duration::from_secs(60));
        let token = downloads
            .issue(
                path.clone(),
                "report.txt".into(),
                "report.txt".into(),
                "text/plain".into(),
                3,
            )
            .await
            .unwrap();
        fs::write(&path, b"current").await.unwrap();
        let download = downloads.open(&token).await.unwrap().unwrap();
        assert_eq!(download.size, 7);
        assert_eq!(download.mime_type, "text/plain");
        assert_eq!(download.file_name, "report.txt");
        assert!(downloads.open(&token).await.unwrap().is_none());
        let expired = Downloads::new(Duration::ZERO);
        let token = expired
            .issue(
                path.clone(),
                "report.txt".into(),
                "report.txt".into(),
                "text/plain".into(),
                7,
            )
            .await
            .unwrap();
        assert!(expired.open(&token).await.unwrap().is_none());
        let token = downloads
            .issue(
                path.clone(),
                "report.txt".into(),
                "report.txt".into(),
                "text/plain".into(),
                7,
            )
            .await
            .unwrap();
        fs::remove_file(&path).await.unwrap();
        fs::create_dir(&path).await.unwrap();
        assert!(downloads.open(&token).await.is_err());
        assert!(downloads.open(&token).await.unwrap().is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn downloads_reject_leaf_and_parent_symlink_replacement() {
        let home = tempfile::tempdir().unwrap();
        let parent = home.path().join("parent");
        fs::create_dir(&parent).await.unwrap();
        let path = parent.join("data");
        let other = home.path().join("other");
        fs::write(&path, b"safe").await.unwrap();
        fs::write(&other, b"private").await.unwrap();
        let downloads = Downloads::new(Duration::from_secs(60));
        let token = downloads
            .issue(
                path.clone(),
                "data".into(),
                "data".into(),
                "text/plain".into(),
                4,
            )
            .await
            .unwrap();
        fs::remove_file(&path).await.unwrap();
        std::os::unix::fs::symlink(&other, &path).unwrap();
        assert!(downloads.open(&token).await.is_err());
        fs::remove_file(&path).await.unwrap();
        fs::write(&path, b"safe").await.unwrap();
        let token = downloads
            .issue(
                path.clone(),
                "data".into(),
                "data".into(),
                "text/plain".into(),
                4,
            )
            .await
            .unwrap();
        let moved = home.path().join("moved");
        fs::rename(&parent, &moved).await.unwrap();
        std::os::unix::fs::symlink(&moved, &parent).unwrap();
        assert!(downloads.open(&token).await.is_err());
    }
}
