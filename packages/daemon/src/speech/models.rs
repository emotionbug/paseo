//! Local speech catalog, configuration and transactional model installation.
//! Catalog URLs do not publish pinned checksums. The install manifest detects local
//! corruption; it is not a claim of publisher-authenticated model provenance.
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{self, BufRead, BufReader, Read, Write},
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
use futures_util::{StreamExt, stream::FuturesUnordered};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;

const MANIFEST: &str = ".paseo-model-integrity.json";

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelSpec {
    pub id: &'static str,
    pub kind: &'static str,
    pub archive_url: &'static str,
    pub extracted_dir: &'static str,
    pub required_files: &'static [&'static str],
    pub description: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_for: Option<&'static str>,
}

pub static MODEL_CATALOG: [ModelSpec; 3] = [
    ModelSpec {
        id: "parakeet-tdt-0.6b-v2-int8",
        kind: "stt-offline",
        archive_url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-nemo-parakeet-tdt-0.6b-v2-int8.tar.bz2",
        extracted_dir: "sherpa-onnx-nemo-parakeet-tdt-0.6b-v2-int8",
        required_files: &[
            "encoder.int8.onnx",
            "decoder.int8.onnx",
            "joiner.int8.onnx",
            "tokens.txt",
        ],
        description: "NVIDIA Parakeet TDT v2 (offline NeMo transducer, English).",
        default_for: Some("stt"),
    },
    ModelSpec {
        id: "parakeet-tdt-0.6b-v3-int8",
        kind: "stt-offline",
        archive_url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8.tar.bz2",
        extracted_dir: "sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8",
        required_files: &[
            "encoder.int8.onnx",
            "decoder.int8.onnx",
            "joiner.int8.onnx",
            "tokens.txt",
        ],
        description: "NVIDIA Parakeet TDT v3 (offline NeMo transducer, 25 European languages, auto-detected).",
        default_for: None,
    },
    ModelSpec {
        id: "kokoro-en-v0_19",
        kind: "tts",
        archive_url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/tts-models/kokoro-en-v0_19.tar.bz2",
        extracted_dir: "kokoro-en-v0_19",
        required_files: &["model.onnx", "voices.bin", "tokens.txt", "espeak-ng-data"],
        description: "Kokoro TTS (higher quality; larger).",
        default_for: Some("tts"),
    },
];

pub fn model_spec(id: &str) -> Result<&'static ModelSpec> {
    MODEL_CATALOG
        .iter()
        .find(|spec| spec.id == id)
        .with_context(|| format!("Unknown local speech model id: {id}"))
}

pub fn model_dir(models_dir: &Path, id: &str) -> Result<PathBuf> {
    Ok(models_dir.join(model_spec(id)?.extracted_dir))
}

#[derive(Clone)]
pub struct ModelStore {
    models_dir: PathBuf,
    client: reqwest::Client,
    integrity: Arc<Mutex<BTreeMap<String, Arc<CachedIntegrity>>>>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct IntegrityManifest {
    version: u32,
    model_id: String,
    archive_url: String,
    files: BTreeMap<PathBuf, FileIntegrity>,
}

#[derive(Serialize, Deserialize)]
struct FileIntegrity {
    size: u64,
    sha256: String,
}

struct CachedIntegrity {
    manifest: Vec<u8>,
    files: Vec<(PathBuf, FileStamp)>,
}

#[derive(PartialEq, Eq)]
struct FileStamp {
    length: u64,
    modified: Option<std::time::SystemTime>,
    created: Option<std::time::SystemTime>,
    #[cfg(unix)]
    identity: (u64, u64, i64, i64),
}

impl FileStamp {
    fn of(metadata: &fs::Metadata) -> Self {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        Self {
            length: metadata.len(),
            modified: metadata.modified().ok(),
            created: metadata.created().ok(),
            #[cfg(unix)]
            identity: (
                metadata.dev(),
                metadata.ino(),
                metadata.ctime(),
                metadata.ctime_nsec(),
            ),
        }
    }
}

impl ModelStore {
    pub fn new(models_dir: PathBuf) -> Self {
        Self {
            models_dir,
            client: reqwest::Client::new(),
            integrity: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    pub fn models_dir(&self) -> &Path {
        &self.models_dir
    }

    pub async fn is_installed(&self, id: &str) -> Result<bool> {
        self.is_installed_cancellable(id, &CancellationToken::new())
            .await
    }

    pub async fn is_installed_cancellable(
        &self,
        id: &str,
        cancel: &CancellationToken,
    ) -> Result<bool> {
        let spec = model_spec(id)?;
        let root = self.models_dir.clone();
        let token = cancel.child_token();
        let _cancel_on_drop = token.clone().drop_guard();
        let integrity = self.integrity.clone();
        tokio::task::spawn_blocking(move || installed(&root, spec, &token, &integrity)).await?
    }

    pub async fn missing_models(&self, ids: &[String]) -> Result<Vec<String>> {
        let mut missing = Vec::new();
        for id in ids {
            if !self.is_installed(id).await? {
                missing.push(id.clone());
            }
        }
        Ok(missing)
    }

    pub async fn ensure_models(
        &self,
        ids: &[String],
        cancel: &CancellationToken,
    ) -> Result<BTreeMap<String, PathBuf>> {
        let batch = cancel.child_token();
        let _cancel_on_drop = batch.clone().drop_guard();
        let mut pending = FuturesUnordered::new();
        for id in ids.iter().collect::<BTreeSet<_>>() {
            let token = batch.clone();
            pending.push(async move {
                Ok::<_, anyhow::Error>((id.clone(), self.ensure(id, &token).await?))
            });
        }
        let mut paths = BTreeMap::new();
        let mut failure = None;
        while let Some(result) = pending.next().await {
            match result {
                Ok((id, path)) => {
                    paths.insert(id, path);
                }
                Err(error) => {
                    if failure.is_none() {
                        failure = Some(error);
                        batch.cancel();
                    }
                }
            }
        }
        if let Some(error) = failure {
            return Err(error);
        }
        Ok(paths)
    }

    pub async fn ensure(&self, id: &str, cancel: &CancellationToken) -> Result<PathBuf> {
        let spec = model_spec(id)?;
        let token = cancel.child_token();
        let _cancel_on_drop = token.clone().drop_guard();
        let store = self.clone();
        // Keep IO and its lock owned until cleanup finishes, even if the caller
        // drops this future while a Tokio filesystem operation is in flight.
        tokio::spawn(async move { store.ensure_inner(spec, token).await }).await?
    }

    async fn ensure_inner(
        &self,
        spec: &'static ModelSpec,
        token: CancellationToken,
    ) -> Result<PathBuf> {
        check_cancel(&token)?;
        let downloads = self.models_dir.join(".downloads");
        tokio::fs::create_dir_all(&downloads).await?;
        let lock_path = downloads.join(format!("{}.lock", spec.extracted_dir));
        let lock = tokio::task::spawn_blocking(move || {
            OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(lock_path)
        })
        .await??;
        loop {
            check_cancel(&token)?;
            match lock.try_lock() {
                Ok(()) => break,
                Err(std::fs::TryLockError::WouldBlock) => tokio::select! {
                    _ = token.cancelled() => bail!("Model download cancelled"),
                    _ = tokio::time::sleep(Duration::from_millis(100)) => {},
                },
                Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
            }
        }
        let root = self.models_dir.clone();
        let check_token = token.clone();
        let integrity = self.integrity.clone();
        let (lock, ready) = tokio::task::spawn_blocking(move || {
            let ready = installed(&root, spec, &check_token, &integrity);
            (lock, ready)
        })
        .await?;
        if ready? {
            return model_dir(&self.models_dir, spec.id);
        }
        let archive = archive_path(&self.models_dir, spec);
        if !nonempty_file(&archive).await? {
            self.download(spec.archive_url, &archive, &token).await?;
        }
        check_cancel(&token)?;
        let root = self.models_dir.clone();
        // The blocking extraction owns the lock and temp directory. Dropping the
        // request cancels its reader without letting another installer race cleanup.
        tokio::task::spawn_blocking(move || {
            let _lock = lock;
            install_archive(&root, spec, &token)
        })
        .await?
    }

    async fn download(&self, url: &str, output: &Path, cancel: &CancellationToken) -> Result<()> {
        let mut response = tokio::select! {
            _ = cancel.cancelled() => bail!("Model download cancelled"),
            result = self.client.get(url).send() => result?,
        };
        ensure!(
            response.status().is_success(),
            "Failed to download {url}: {}",
            response.status()
        );
        let directory = output
            .parent()
            .context("Missing archive directory")?
            .to_owned();
        let temporary =
            tokio::task::spawn_blocking(move || tempfile::NamedTempFile::new_in(directory))
                .await??;
        let (file, temp_path) = temporary.into_parts();
        let mut file = tokio::fs::File::from_std(file);
        loop {
            let chunk = tokio::select! {
                _ = cancel.cancelled() => bail!("Model download cancelled"),
                result = response.chunk() => result?,
            };
            let Some(chunk) = chunk else { break };
            tokio::select! {
                _ = cancel.cancelled() => bail!("Model download cancelled"),
                result = file.write_all(&chunk) => result?,
            }
        }
        file.sync_all().await?;
        drop(file);
        check_cancel(cancel)?;
        ensure!(
            tokio::fs::metadata(&temp_path).await?.len() > 0,
            "Failed to download {url}: empty response body"
        );
        tokio::fs::rename(&temp_path, output).await?;
        let directory = output.parent().unwrap().to_owned();
        tokio::task::spawn_blocking(move || sync_directory(&directory)).await??;
        Ok(())
    }
}

fn archive_path(root: &Path, spec: &ModelSpec) -> PathBuf {
    root.join(".downloads")
        .join(spec.archive_url.rsplit('/').next().unwrap())
}

async fn nonempty_file(path: &Path) -> Result<bool> {
    match tokio::fs::symlink_metadata(path).await {
        Ok(meta) => Ok(meta.is_file() && meta.len() > 0),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn check_cancel(cancel: &CancellationToken) -> Result<()> {
    ensure!(!cancel.is_cancelled(), "Model download cancelled");
    Ok(())
}

fn required_files_present(root: &Path, spec: &ModelSpec) -> bool {
    if !fs::symlink_metadata(root).is_ok_and(|m| m.is_dir()) {
        return false;
    }
    spec.required_files.iter().all(|name| {
        fs::symlink_metadata(root.join(name)).is_ok_and(|m| {
            if *name == "espeak-ng-data" {
                m.is_dir()
            } else {
                m.is_file() && m.len() > 0
            }
        })
    })
}

fn installed(
    root: &Path,
    spec: &ModelSpec,
    cancel: &CancellationToken,
    integrity: &Mutex<BTreeMap<String, Arc<CachedIntegrity>>>,
) -> Result<bool> {
    check_cancel(cancel)?;
    if fs::symlink_metadata(archive_path(root, spec)).is_ok_and(|m| m.is_file() && m.len() > 0) {
        return Ok(false);
    }
    let dir = root.join(spec.extracted_dir);
    if !required_files_present(&dir, spec) {
        return Ok(false);
    }
    let bytes = match fs::read(dir.join(MANIFEST)) {
        Ok(bytes) => bytes,
        // Existing source installations have no manifest. Preserve their exact
        // required-file readiness rule; all new installs carry integrity records.
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(true),
        Err(error) => return Err(error.into()),
    };
    let cached = integrity
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(spec.id)
        .cloned();
    if let Some(cached) = cached {
        if cached.manifest == bytes
            && cached.files.iter().all(|(path, stamp)| {
                regular_path(&dir, path)
                    && fs::symlink_metadata(dir.join(path))
                        .is_ok_and(|meta| FileStamp::of(&meta) == *stamp)
            })
        {
            return Ok(true);
        }
    }
    let Ok(manifest) = serde_json::from_slice::<IntegrityManifest>(&bytes) else {
        return Ok(false);
    };
    if manifest.version != 1
        || manifest.model_id != spec.id
        || manifest.archive_url != spec.archive_url
        || manifest.files.is_empty()
    {
        return Ok(false);
    }
    for name in spec
        .required_files
        .iter()
        .filter(|name| **name != "espeak-ng-data")
    {
        if !manifest.files.contains_key(Path::new(name)) {
            return Ok(false);
        }
    }
    let mut buffer = [0u8; 64 * 1024];
    let mut files = Vec::with_capacity(manifest.files.len());
    for (relative, expected) in manifest.files {
        check_cancel(cancel)?;
        if !safe_relative(&relative) || !regular_path(&dir, &relative) {
            return Ok(false);
        }
        let mut file = match File::open(dir.join(&relative)) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error.into()),
        };
        let before = FileStamp::of(&file.metadata()?);
        if before.length != expected.size {
            return Ok(false);
        }
        let mut hash = Sha256::new();
        loop {
            check_cancel(cancel)?;
            let n = file.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            hash.update(&buffer[..n]);
        }
        if hex::encode(hash.finalize()) != expected.sha256 {
            return Ok(false);
        }
        if FileStamp::of(&file.metadata()?) != before
            || !fs::symlink_metadata(dir.join(&relative))
                .is_ok_and(|meta| FileStamp::of(&meta) == before)
        {
            return Ok(false);
        }
        files.push((relative, before));
    }
    integrity.lock().unwrap_or_else(|e| e.into_inner()).insert(
        spec.id.to_owned(),
        Arc::new(CachedIntegrity {
            manifest: bytes,
            files,
        }),
    );
    Ok(true)
}

fn safe_relative(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && path.components().all(|part| match part {
            Component::Normal(name) => name.to_str().is_some_and(|s| !s.contains(['\\', ':'])),
            _ => false,
        })
}

fn regular_path(root: &Path, path: &Path) -> bool {
    let mut current = root.to_owned();
    for part in path.components() {
        current.push(part);
        if !fs::symlink_metadata(&current).is_ok_and(|m| !m.file_type().is_symlink()) {
            return false;
        }
    }
    fs::symlink_metadata(current).is_ok_and(|m| m.is_file())
}

struct CancelRead<R> {
    inner: R,
    cancel: CancellationToken,
}
impl<R: Read> Read for CancelRead<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.cancel.is_cancelled() {
            return Err(io::Error::other("Model download cancelled"));
        }
        self.inner.read(buffer)
    }
}

fn install_archive(root: &Path, spec: &ModelSpec, cancel: &CancellationToken) -> Result<PathBuf> {
    check_cancel(cancel)?;
    let archive_path = archive_path(root, spec);
    let downloads = root.join(".downloads");
    let staging = tempfile::Builder::new()
        .prefix(&format!("{}.extracting-", spec.extracted_dir))
        .tempdir_in(&downloads)?;
    let file = CancelRead {
        inner: File::open(&archive_path)?,
        cancel: cancel.clone(),
    };
    let mut reader = BufReader::new(file);
    let magic = reader.fill_buf()?;
    let decoded: Box<dyn Read> = if magic.starts_with(b"BZh") {
        Box::new(bzip2::read::BzDecoder::new(reader))
    } else if magic.starts_with(&[0x1f, 0x8b]) {
        Box::new(flate2::read::GzDecoder::new(reader))
    } else {
        Box::new(reader)
    };
    let mut archive = tar::Archive::new(decoded);
    let mut manifest = IntegrityManifest {
        version: 1,
        model_id: spec.id.to_owned(),
        archive_url: spec.archive_url.to_owned(),
        files: BTreeMap::new(),
    };
    let mut buffer = [0u8; 64 * 1024];
    for entry in archive.entries()? {
        check_cancel(cancel)?;
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        ensure!(
            safe_relative(&path),
            "Unsafe archive path: {}",
            path.display()
        );
        let relative = path.strip_prefix(spec.extracted_dir).with_context(|| {
            format!("Archive entry outside model directory: {}", path.display())
        })?;
        ensure!(
            relative != Path::new(MANIFEST),
            "Archive contains reserved integrity manifest"
        );
        let kind = entry.header().entry_type();
        ensure!(
            kind.is_file() || kind.is_dir(),
            "Unsafe archive entry type: {}",
            path.display()
        );
        let destination = staging.path().join(&path);
        if kind.is_dir() {
            fs::create_dir_all(&destination)?;
            continue;
        }
        ensure!(
            !relative.as_os_str().is_empty(),
            "Model directory is a file"
        );
        fs::create_dir_all(
            destination
                .parent()
                .context("Missing archive entry parent")?,
        )?;
        let mut output = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&destination)?;
        let mut hash = Sha256::new();
        let mut size = 0u64;
        loop {
            check_cancel(cancel)?;
            let n = entry.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            output.write_all(&buffer[..n])?;
            hash.update(&buffer[..n]);
            size += n as u64;
        }
        output.sync_all()?;
        manifest.files.insert(
            relative.to_owned(),
            FileIntegrity {
                size,
                sha256: hex::encode(hash.finalize()),
            },
        );
    }
    // Read the codec to EOF as tar may stop at its end marker before the
    // compressed stream checksum/trailer. Corrupt trailers must not publish.
    let mut decoded = archive.into_inner();
    loop {
        check_cancel(cancel)?;
        if decoded.read(&mut buffer)? == 0 {
            break;
        }
    }
    let extracted = staging.path().join(spec.extracted_dir);
    ensure!(
        required_files_present(&extracted, spec),
        "Downloaded and extracted {}, but required files are missing.",
        archive_path.file_name().unwrap().to_string_lossy()
    );
    let mut record = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(extracted.join(MANIFEST))?;
    serde_json::to_writer(&mut record, &manifest)?;
    record.sync_all()?;
    drop(record);
    sync_tree_directories(&extracted)?;
    check_cancel(cancel)?;
    let destination = root.join(spec.extracted_dir);
    // Keep the source archive marker until all assets and their manifest are
    // durable. Removing it before rename preserves legacy interrupted-install
    // detection without ever exposing an incomplete destination.
    remove_destination(&destination)?;
    fs::remove_file(&archive_path)?;
    sync_directory(&downloads)?;
    fs::rename(&extracted, &destination)?;
    sync_directory(root)?;
    Ok(destination)
}

fn remove_destination(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => fs::remove_dir_all(path)?,
        Ok(_) => fs::remove_file(path)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

fn sync_tree_directories(root: &Path) -> Result<()> {
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            sync_tree_directories(&entry.path())?;
        }
    }
    sync_directory(root)
}

fn sync_directory(path: &Path) -> Result<()> {
    #[cfg(unix)]
    File::open(path)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEFAULT_LOCAL_TTS_MODEL: &str = "kokoro-en-v0_19";

    fn archive_fixture(root: &Path, link: bool) -> Result<()> {
        let spec = model_spec(DEFAULT_LOCAL_TTS_MODEL)?;
        let archive = archive_path(root, spec);
        fs::create_dir_all(archive.parent().unwrap())?;
        let compressed =
            bzip2::write::BzEncoder::new(File::create(archive)?, bzip2::Compression::default());
        let mut builder = tar::Builder::new(compressed);
        for name in spec.required_files {
            let mut header = tar::Header::new_gnu();
            header.set_mode(0o644);
            if *name == "espeak-ng-data" {
                header.set_entry_type(tar::EntryType::Directory);
                header.set_size(0);
                header.set_cksum();
                builder.append_data(
                    &mut header,
                    format!("{}/{name}", spec.extracted_dir),
                    io::empty(),
                )?;
            } else {
                let bytes = b"complete model data";
                header.set_size(bytes.len() as u64);
                header.set_cksum();
                builder.append_data(
                    &mut header,
                    format!("{}/{name}", spec.extracted_dir),
                    &bytes[..],
                )?;
            }
        }
        if link {
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Symlink);
            header.set_size(0);
            header.set_mode(0o777);
            header.set_link_name("../../outside")?;
            header.set_cksum();
            builder.append_data(
                &mut header,
                format!("{}/escape", spec.extracted_dir),
                io::empty(),
            )?;
        }
        builder.into_inner()?.finish()?.sync_all()?;
        Ok(())
    }

    #[tokio::test]
    async fn retained_archive_repairs_legacy_and_manifest_detects_same_size_corruption()
    -> Result<()> {
        let temp = tempfile::tempdir()?;
        archive_fixture(temp.path(), false)?;
        let spec = model_spec(DEFAULT_LOCAL_TTS_MODEL)?;
        let dir = model_dir(temp.path(), spec.id)?;
        fs::create_dir_all(dir.join("espeak-ng-data"))?;
        for name in ["model.onnx", "voices.bin", "tokens.txt"] {
            fs::write(dir.join(name), b"incomplete")?;
        }
        let store = ModelStore::new(temp.path().to_owned());
        assert!(!store.is_installed(spec.id).await?);
        assert_eq!(store.ensure(spec.id, &CancellationToken::new()).await?, dir);
        assert!(store.is_installed(spec.id).await?);
        assert!(!archive_path(temp.path(), spec).exists());
        let length = fs::metadata(dir.join("model.onnx"))?.len() as usize;
        fs::write(dir.join("model.onnx"), vec![b'x'; length])?;
        assert!(!store.is_installed(spec.id).await?);
        Ok(())
    }

    #[tokio::test]
    async fn unsafe_archive_never_publishes_and_retains_recovery_marker() -> Result<()> {
        let temp = tempfile::tempdir()?;
        archive_fixture(temp.path(), true)?;
        let spec = model_spec(DEFAULT_LOCAL_TTS_MODEL)?;
        let store = ModelStore::new(temp.path().to_owned());
        let error = store
            .ensure(spec.id, &CancellationToken::new())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("Unsafe archive entry type"));
        assert!(!model_dir(temp.path(), spec.id)?.exists());
        assert!(archive_path(temp.path(), spec).exists());
        assert!(!store.is_installed(spec.id).await?);
        assert!(fs::read_dir(temp.path().join(".downloads"))?.all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".extracting-")
        }));
        Ok(())
    }

    #[tokio::test]
    async fn cancelled_install_keeps_archive_and_no_destination() -> Result<()> {
        let temp = tempfile::tempdir()?;
        archive_fixture(temp.path(), false)?;
        let spec = model_spec(DEFAULT_LOCAL_TTS_MODEL)?;
        let token = CancellationToken::new();
        token.cancel();
        let store = ModelStore::new(temp.path().to_owned());
        assert!(store.ensure(spec.id, &token).await.is_err());
        assert!(archive_path(temp.path(), spec).exists());
        assert!(!model_dir(temp.path(), spec.id)?.exists());
        Ok(())
    }

    #[test]
    fn archive_paths_cannot_escape_or_use_platform_aliases() {
        for path in [
            "../outside",
            "/absolute",
            "model/../../outside",
            "model\\outside",
            "C:outside",
        ] {
            assert!(!safe_relative(Path::new(path)), "{path}");
        }
        assert!(safe_relative(Path::new("model/espeak-ng-data/lang/en")));
    }
}
