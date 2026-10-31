//! Updates only owned standalone runtime archives, never npm or a desktop bundle.
//! The release hash covers the executable, libraries, skills and plugin runtime together.
use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha512};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use url::Url;

#[cfg(windows)]
#[path = "updater_windows.rs"]
mod windows;
#[cfg(windows)]
pub(crate) use windows::{
    cleanup_update_helper, launch_deferred_update, launch_recovery_helper, run_update_helper,
};
const MAX_ARCHIVE: u64 = 1024 * 1024 * 1024;
const MAX_EXPANDED: u64 = 8 * 1024 * 1024 * 1024;
const MARKER: &str = "paseo-runtime.json";
const RECORD: &str = "native-update.json";
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    pub version: String,
    tag: String,
    source_sha: String,
    platform: String,
    arch: String,
    format: String,
    name: String,
    size: u64,
    digest: String,
    url: Url,
    manifest_digest: String,
}
pub struct Prepared {
    pub release: Release,
    directory: tempfile::TempDir,
    staged: PathBuf,
    target: PathBuf,
    original_digest: String,
    new_digest: String,
    _lease: File,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Transaction {
    target: PathBuf,
    staged: PathBuf,
    backup: PathBuf,
    directory: PathBuf,
    original_digest: String,
    new_digest: String,
    version: String,
    source_sha: String,
    #[serde(default)]
    committed: bool,
    #[serde(default)]
    deferred: bool,
}

fn platform() -> Result<(&'static str, &'static str)> {
    let platform = match std::env::consts::OS {
        "linux" => "linux",
        "macos" => "macos",
        "windows" => "windows",
        _ => bail!("Native updates are unsupported on this platform"),
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        _ => bail!("Native updates are unsupported on this architecture"),
    };
    Ok((platform, arch))
}
fn repository() -> Result<&'static str> {
    let repository = option_env!("PASEO_UPDATE_REPOSITORY")
        .context("This build has no configured native update repository")?;
    let parts = repository.split('/').collect::<Vec<_>>();
    if parts.len() != 2
        || parts.iter().any(|s| {
            s.is_empty()
                || !s
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c))
        })
    {
        bail!("Invalid configured update repository")
    };
    Ok(repository)
}
fn channel() -> Result<&'static str> {
    let channel = option_env!("PASEO_RELEASE_CHANNEL").unwrap_or(
        if env!("CARGO_PKG_VERSION").contains('-') {
            "beta"
        } else {
            "stable"
        },
    );
    if !matches!(channel, "stable" | "beta") {
        bail!("Invalid configured release channel")
    };
    Ok(channel)
}
fn http() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .user_agent(concat!("paseo-native/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(180))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            let url = attempt.url();
            let trusted = url.host_str().is_some_and(|h| {
                matches!(
                    h,
                    "github.com"
                        | "api.github.com"
                        | "release-assets.githubusercontent.com"
                        | "objects.githubusercontent.com"
                )
            });
            if attempt.previous().len() > 5
                || url.scheme() != "https"
                || !url.username().is_empty()
                || url.password().is_some()
                || !trusted
            {
                attempt.error("Untrusted update redirect")
            } else {
                attempt.follow()
            }
        }))
        .build()?)
}
async fn json_bytes(client: &reqwest::Client, url: Url) -> Result<Vec<u8>> {
    let mut response = client.get(url).send().await?.error_for_status()?;
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if bytes.len() + chunk.len() > 4 * 1024 * 1024 {
            bail!("Release metadata exceeds size limit")
        };
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}
fn asset_url(asset: &Value, repository: &str, tag: &str, name: &str) -> Result<Url> {
    let url = Url::parse(
        asset["browser_download_url"]
            .as_str()
            .context("Missing asset URL")?,
    )?;
    let parts = url
        .path_segments()
        .context("Invalid asset URL")?
        .map(|s| {
            percent_encoding::percent_decode_str(s)
                .decode_utf8()
                .map(|v| v.into_owned())
        })
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut expected = repository.split('/').map(str::to_owned).collect::<Vec<_>>();
    expected.extend([
        "releases".into(),
        "download".into(),
        tag.into(),
        name.into(),
    ]);
    if url.scheme() != "https"
        || url.host_str() != Some("github.com")
        || url.port_or_known_default() != Some(443)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || parts != expected
    {
        bail!("Release asset is not the exact configured repository/tag/name")
    };
    Ok(url)
}
fn unique_asset<'a>(release: &'a Value, name: &str) -> Result<&'a Value> {
    let assets = release["assets"]
        .as_array()
        .context("Missing release assets")?;
    let mut found = assets.iter().filter(|v| v["name"].as_str() == Some(name));
    let asset = found
        .next()
        .with_context(|| format!("Missing release asset {name}"))?;
    if found.next().is_some() {
        bail!("Ambiguous release asset {name}")
    };
    Ok(asset)
}
pub(super) fn validate_manifest(
    manifest: &Value,
    release: &Value,
    repository: &str,
    channel: &str,
    platform: &str,
    arch: &str,
    manifest_digest: String,
) -> Result<Release> {
    if manifest["schemaVersion"] != 1
        || manifest["runtime"] != "rust"
        || manifest["appId"] != "sh.paseo.daemon"
        || manifest["channel"] != channel
        || manifest["platform"] != platform
        || manifest["arch"] != arch
    {
        bail!("Native release identity mismatch")
    }
    let version = manifest["version"]
        .as_str()
        .context("Missing release version")?;
    let parsed = semver::Version::parse(version)?;
    if channel == "stable" && !parsed.pre.is_empty() {
        bail!("Stable release contains a prerelease version")
    }
    let tag = release["tag_name"]
        .as_str()
        .context("Missing release tag")?;
    if tag != format!("v{version}") && tag != version {
        bail!("Manifest version does not match the published release tag")
    }
    let source = manifest["sourceSha"]
        .as_str()
        .context("Missing native source provenance")?;
    if !matches!(source.len(), 40 | 64) || !source.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("Invalid native source provenance")
    }
    let artifact = &manifest["artifact"];
    let format = if platform == "windows" {
        "zip"
    } else {
        "tar.gz"
    };
    let name = artifact["name"]
        .as_str()
        .context("Missing native artifact name")?;
    if name != format!("Paseo-Daemon-{version}-{platform}-{arch}.{format}")
        || artifact["format"] != format
    {
        bail!("Wrong native runtime archive name or format")
    }
    let size = artifact["size"]
        .as_u64()
        .filter(|n| *n > 0 && *n <= MAX_ARCHIVE)
        .context("Invalid artifact size")?;
    let digest = artifact["sha512"]
        .as_str()
        .context("Missing artifact SHA512")?;
    if STANDARD.decode(digest)?.len() != 64 {
        bail!("Invalid artifact SHA512")
    }
    let asset = unique_asset(release, name)?;
    if asset["size"].as_u64() != Some(size) {
        bail!("Published artifact size mismatch")
    }
    Ok(Release {
        version: version.into(),
        tag: tag.into(),
        source_sha: source.to_ascii_lowercase(),
        platform: platform.into(),
        arch: arch.into(),
        format: format.into(),
        name: name.into(),
        size,
        digest: digest.into(),
        url: asset_url(asset, repository, tag, name)?,
        manifest_digest,
    })
}
async fn discover(client: &reqwest::Client) -> Result<Release> {
    let (repository, channel) = (repository()?, channel()?);
    let (platform, arch) = platform()?;
    let name = format!("daemon-{channel}-{platform}-{arch}.json");
    let mut newest: Option<Release> = None;
    for page in 1..=10 {
        let values: Value = serde_json::from_slice(
            &json_bytes(
                client,
                Url::parse(&format!(
                    "https://api.github.com/repos/{repository}/releases?per_page=100&page={page}"
                ))?,
            )
            .await?,
        )?;
        let values = values
            .as_array()
            .context("GitHub did not return a release list")?;
        for release in values {
            if release["draft"] == true || (channel == "stable" && release["prerelease"] == true) {
                continue;
            }
            if !release["assets"]
                .as_array()
                .is_some_and(|assets| assets.iter().any(|a| a["name"] == name))
            {
                continue;
            }
            let asset = unique_asset(release, &name)?;
            let tag = release["tag_name"]
                .as_str()
                .context("Missing release tag")?;
            let url = asset_url(asset, repository, tag, &name)?;
            let bytes = json_bytes(client, url).await?;
            let digest = STANDARD.encode(Sha512::digest(&bytes));
            let manifest: Value = serde_json::from_slice(&bytes)?;
            let candidate = validate_manifest(
                &manifest, release, repository, channel, platform, arch, digest,
            )?;
            if newest.as_ref().is_none_or(|old| {
                semver::Version::parse(&candidate.version).unwrap()
                    > semver::Version::parse(&old.version).unwrap()
            }) {
                newest = Some(candidate)
            }
        }
        if values.len() < 100 {
            break;
        }
    }
    let selected = newest.with_context(|| {
        format!("No {channel} native release is published for {platform}/{arch} in {repository}")
    })?;
    verify_source_commit(client, repository, &selected).await?;
    Ok(selected)
}
async fn verify_source_commit(
    client: &reqwest::Client,
    repository: &str,
    release: &Release,
) -> Result<()> {
    let mut object: Value = serde_json::from_slice(
        &json_bytes(
            client,
            Url::parse(&format!(
                "https://api.github.com/repos/{repository}/git/ref/tags/{}",
                release.tag
            ))?,
        )
        .await?,
    )?;
    for _ in 0..8 {
        let sha = object["object"]["sha"]
            .as_str()
            .context("Release tag has no source object")?;
        if !matches!(sha.len(), 40 | 64) || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
            bail!("Invalid release tag object identity")
        }
        match object["object"]["type"].as_str() {
            Some("commit") => {
                if !sha.eq_ignore_ascii_case(&release.source_sha) {
                    bail!("Release tag commit does not match native source provenance")
                }
                return Ok(());
            }
            Some("tag") => {
                object = serde_json::from_slice(
                    &json_bytes(
                        client,
                        Url::parse(&format!(
                            "https://api.github.com/repos/{repository}/git/tags/{sha}"
                        ))?,
                    )
                    .await?,
                )?;
            }
            _ => bail!("Release tag does not resolve to a source commit"),
        }
    }
    bail!("Release annotated-tag chain exceeds the provenance bound")
}
fn executable_name(platform: &str) -> &'static str {
    if platform == "windows" {
        "paseo.exe"
    } else {
        "paseo"
    }
}
fn read_marker(root: &Path) -> Result<Value> {
    let path = root.join(MARKER);
    let metadata = fs::symlink_metadata(&path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 64 * 1024 {
        bail!("Invalid installed runtime identity marker")
    };
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}
pub(super) fn validate_marker(
    marker: &Value,
    version: &str,
    source: &str,
    platform: &str,
    arch: &str,
) -> Result<()> {
    if marker["schemaVersion"] != 1
        || marker["runtime"] != "rust"
        || marker["appId"] != "sh.paseo.daemon"
        || marker["installationKind"] != "standalone-archive"
        || marker["version"] != version
        || marker["sourceSha"] != source
        || marker["platform"] != platform
        || marker["arch"] != arch
    {
        bail!(
            "Runtime ownership/version/provenance mismatch; update through the installation's package manager or desktop application"
        )
    };
    Ok(())
}
fn lease(root: &Path) -> Result<File> {
    let parent = root
        .parent()
        .context("Runtime installation has no parent")?;
    let name = root
        .file_name()
        .context("Runtime installation has no name")?
        .to_string_lossy();
    let path = parent.join(format!(".paseo-{name}.update.lock"));
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let file = options.open(&path)?;
    if !file.metadata()?.is_file() || fs::symlink_metadata(path)?.file_type().is_symlink() {
        bail!("Invalid native update lock")
    };
    file.try_lock()
        .context("Another process is updating this native installation")?;
    Ok(file)
}
pub async fn prepare(executable: &Path, desktop_managed: bool) -> Result<Prepared> {
    if desktop_managed
        || executable
            .components()
            .any(|c| c.as_os_str().to_string_lossy().ends_with(".app"))
    {
        bail!("This daemon is managed by Paseo Desktop. Update Paseo Desktop on the host.")
    }
    if option_env!("PASEO_NATIVE_RELEASE") != Some("1") {
        bail!(
            "This build has no native release provenance. Install a packaged standalone native release."
        )
    }
    let source = option_env!("PASEO_BUILD_SOURCE_SHA")
        .context("This build has no native source provenance")?;
    let executable = executable.canonicalize()?;
    let target = executable
        .parent()
        .context("Executable has no runtime root")?
        .to_owned();
    let (platform, arch) = platform()?;
    if executable.file_name() != Some(std::ffi::OsStr::new(executable_name(platform))) {
        bail!("Installed executable is not the standalone native runtime entry")
    }
    let marker = read_marker(&target)?;
    if marker["installationKind"] == "desktop-bundled" {
        bail!("This daemon is managed by Paseo Desktop. Update Paseo Desktop on the host.")
    }
    validate_marker(&marker, env!("CARGO_PKG_VERSION"), source, platform, arch)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = fs::metadata(&target)?;
        if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o022 != 0 {
            bail!("Runtime root has unsafe ownership or permissions")
        }
    }
    let lease = lease(&target)?;
    let client = http()?;
    let release = discover(&client).await?;
    if semver::Version::parse(&release.version)?
        <= semver::Version::parse(env!("CARGO_PKG_VERSION"))?
    {
        bail!("No newer native daemon release is available")
    }
    let directory = tempfile::Builder::new()
        .prefix(".paseo-update-")
        .tempdir_in(target.parent().context("Runtime root has no parent")?)?;
    let archive = directory.path().join(&release.name);
    let staged = directory.path().join("runtime");
    let mut file = tokio::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&archive)
        .await?;
    let response = client
        .get(release.url.clone())
        .send()
        .await?
        .error_for_status()?;
    if response.content_length().is_some_and(|n| n != release.size) {
        bail!("Download content length mismatch")
    }
    let mut stream = response.bytes_stream();
    let mut hash = Sha512::new();
    let mut size = 0u64;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        size = size
            .checked_add(chunk.len() as u64)
            .context("Artifact size overflow")?;
        if size > release.size {
            bail!("Downloaded artifact exceeds declared size")
        };
        hash.update(&chunk);
        file.write_all(&chunk).await?;
    }
    file.sync_all().await?;
    drop(file);
    if size != release.size || STANDARD.encode(hash.finalize()) != release.digest {
        bail!("Downloaded update failed SHA512/size verification")
    }
    let (archive_path, staged_path, format) =
        (archive.clone(), staged.clone(), release.format.clone());
    tokio::task::spawn_blocking(move || extract(&archive_path, &staged_path, &format)).await??;
    validate_marker(
        &read_marker(&staged)?,
        &release.version,
        &release.source_sha,
        &release.platform,
        &release.arch,
    )?;
    for name in ["lib", "skills"] {
        if !staged.join(name).is_dir() {
            bail!("Native runtime archive is missing {name}/")
        }
    }
    let new_executable = staged.join(executable_name(platform));
    validate_binary(&new_executable).await?;
    validate_signature(&executable, &new_executable).await?;
    let (original_digest, new_digest) = tokio::try_join!(
        tree_digest_async(target.clone()),
        tree_digest_async(staged.clone())
    )?;
    tokio::fs::remove_file(archive).await?;
    Ok(Prepared {
        release,
        directory,
        staged,
        target,
        original_digest,
        new_digest,
        _lease: lease,
    })
}

fn safe_member(path: &Path) -> Result<PathBuf> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
    {
        bail!("Unsafe runtime archive path")
    }
    let mut normalized = PathBuf::new();
    for part in path.components() {
        if let Component::Normal(part) = part {
            let text = part.to_str().context("Non-UTF8 archive path")?;
            let stem = text.split('.').next().unwrap_or("").to_ascii_uppercase();
            let reserved = matches!(
                stem.as_str(),
                "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
            ) || stem
                .strip_prefix("COM")
                .or_else(|| stem.strip_prefix("LPT"))
                .is_some_and(|suffix| {
                    matches!(suffix, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
                });
            if reserved
                || text.chars().any(|c| c < ' ' || "\\:<>\"|?*".contains(c))
                || text.ends_with('.')
                || text.ends_with(' ')
            {
                bail!("Unsafe cross-platform archive path")
            }
            normalized.push(part);
        }
    }
    if normalized.as_os_str().is_empty() {
        bail!("Empty runtime archive member")
    };
    Ok(normalized)
}
fn write_member(
    root: &Path,
    name: &Path,
    reader: &mut impl Read,
    size: u64,
    mode: u32,
) -> Result<()> {
    let path = root.join(safe_member(name)?);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?
    }
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(if mode & 0o111 != 0 { 0o755 } else { 0o644 });
    }
    let mut file = options.open(path)?;
    let copied = std::io::copy(&mut reader.take(size + 1), &mut file)?;
    if copied != size {
        bail!("Runtime archive member size mismatch")
    };
    file.sync_all()?;
    Ok(())
}
pub(super) fn extract(archive: &Path, root: &Path, format: &str) -> Result<()> {
    fs::create_dir(root)?;
    let mut expanded = 0u64;
    let mut names = std::collections::HashSet::new();
    let mut count = 0usize;
    if format == "tar.gz" {
        let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(File::open(archive)?));
        for entry in archive.entries()? {
            let mut entry = entry?;
            let kind = entry.header().entry_type();
            let raw = entry.path()?.into_owned();
            if kind.is_dir()
                && raw
                    .components()
                    .all(|part| matches!(part, Component::CurDir))
            {
                continue;
            }
            let path = safe_member(&raw)?;
            count += 1;
            if count > 100_000 || !names.insert(path.clone()) {
                bail!("Duplicate or excessive runtime archive members")
            }
            if kind.is_dir() {
                fs::create_dir_all(root.join(path))?;
                continue;
            }
            if !kind.is_file() {
                bail!("Runtime archives cannot contain links or special files")
            }
            let size = entry.size();
            expanded = expanded
                .checked_add(size)
                .context("Archive size overflow")?;
            if expanded > MAX_EXPANDED {
                bail!("Expanded runtime exceeds size limit")
            };
            let mode = entry.header().mode()?;
            write_member(root, &path, &mut entry, size, mode)?;
        }
    } else if format == "zip" {
        let mut archive = zip::ZipArchive::new(File::open(archive)?)?;
        if archive.len() > 100_000 {
            bail!("Excessive runtime archive members")
        }
        for index in 0..archive.len() {
            let mut entry = archive.by_index(index)?;
            let path = safe_member(Path::new(entry.name()))?;
            let mode = entry.unix_mode().unwrap_or(0o644);
            if mode & 0o170000 != 0 && mode & 0o170000 != 0o100000 && mode & 0o170000 != 0o040000 {
                bail!("Runtime archives cannot contain links or special files")
            };
            if !names.insert(path.clone()) {
                bail!("Duplicate runtime archive member")
            }
            if entry.is_dir() {
                fs::create_dir_all(root.join(path))?;
                continue;
            }
            let size = entry.size();
            expanded = expanded
                .checked_add(size)
                .context("Archive size overflow")?;
            if expanded > MAX_EXPANDED {
                bail!("Expanded runtime exceeds size limit")
            };
            write_member(root, &path, &mut entry, size, mode)?;
        }
    } else {
        bail!("Unsupported native runtime archive format")
    }
    Ok(())
}
async fn tree_digest_async(root: PathBuf) -> Result<String> {
    tokio::task::spawn_blocking(move || tree_digest(&root)).await?
}
fn tree_digest(root: &Path) -> Result<String> {
    if fs::symlink_metadata(root)?.file_type().is_symlink() {
        bail!("Runtime root cannot be a symlink")
    }
    let mut paths = walkdir::WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .collect::<std::result::Result<Vec<_>, _>>()?;
    paths.sort_by(|a, b| a.path().cmp(b.path()));
    let mut hash = Sha512::new();
    let mut buffer = [0u8; 65536];
    for entry in paths {
        let relative = entry.path().strip_prefix(root)?;
        if relative.as_os_str().is_empty() {
            continue;
        }
        let metadata = fs::symlink_metadata(entry.path())?;
        if !metadata.is_dir() && !metadata.is_file() {
            bail!(
                "Runtime contains a symlink or special file: {}",
                relative.display()
            )
        };
        let name = relative
            .to_str()
            .context("Non-UTF8 runtime path")?
            .replace('\\', "/");
        hash.update((name.len() as u64).to_le_bytes());
        hash.update(name.as_bytes());
        hash.update([if metadata.is_dir() { 0 } else { 1 }]);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            hash.update((metadata.permissions().mode() & 0o777).to_le_bytes());
        }
        if metadata.is_file() {
            hash.update(metadata.len().to_le_bytes());
            let mut file = File::open(entry.path())?;
            loop {
                let count = file.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                hash.update(&buffer[..count]);
            }
        }
    }
    Ok(STANDARD.encode(hash.finalize()))
}
async fn verify_tree(path: &Path, expected: &str) -> Result<()> {
    if tree_digest_async(path.to_owned()).await? != expected {
        bail!("Native runtime files changed: {}", path.display())
    };
    Ok(())
}
async fn write_record(
    home: &Path,
    transaction: &Transaction,
    replace_existing: bool,
) -> Result<()> {
    let (home, bytes) = (home.to_owned(), serde_json::to_vec(transaction)?);
    tokio::task::spawn_blocking(move || {
        let mut file = tempfile::NamedTempFile::new_in(&home)?;
        file.write_all(&bytes)?;
        file.as_file().sync_all()?;
        if replace_existing {
            file.persist(home.join(RECORD)).map_err(|e| e.error)?;
        } else {
            file.persist_noclobber(home.join(RECORD))
                .map_err(|e| e.error)?;
        }
        #[cfg(unix)]
        File::open(home)?.sync_all()?;
        Ok::<_, anyhow::Error>(())
    })
    .await?
}

pub async fn install(home: &Path, prepared: Prepared) -> Result<String> {
    if home.canonicalize()?.starts_with(&prepared.target) {
        bail!("Daemon home cannot be inside the replaceable runtime installation")
    }
    if discover(&http()?).await? != prepared.release {
        bail!("The release changed during download. Check for updates again.")
    }
    verify_tree(&prepared.staged, &prepared.new_digest).await?;
    verify_tree(&prepared.target, &prepared.original_digest).await?;
    validate_signature(
        &prepared
            .target
            .join(executable_name(&prepared.release.platform)),
        &prepared
            .staged
            .join(executable_name(&prepared.release.platform)),
    )
    .await?;
    let transaction = Transaction {
        target: prepared.target,
        staged: prepared.staged,
        backup: prepared.directory.path().join("previous"),
        directory: prepared.directory.path().to_owned(),
        original_digest: prepared.original_digest,
        new_digest: prepared.new_digest,
        version: prepared.release.version,
        source_sha: prepared.release.source_sha,
        committed: false,
        deferred: cfg!(windows),
    };
    write_record(home, &transaction, false).await?;
    let _directory = prepared.directory.keep();
    if transaction.deferred {
        return Ok(transaction.version);
    }
    if let Err(error) = replace(&transaction).await {
        let recovery = rollback(&transaction).await;
        if recovery.is_ok() {
            cleanup(home, &transaction).await?;
        }
        return match recovery {
            Ok(()) => Err(error),
            Err(recovery) => Err(error.context(format!(
                "Update recovery failed: {recovery}; retained {}",
                home.join(RECORD).display()
            ))),
        };
    }
    Ok(transaction.version)
}
async fn replace(t: &Transaction) -> Result<()> {
    tokio::fs::rename(&t.target, &t.backup)
        .await
        .context("Cannot preserve installed runtime")?;
    tokio::fs::rename(&t.staged, &t.target)
        .await
        .context("Cannot install verified runtime")?;
    sync_parent(&t.target).await
}
async fn sync_parent(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        let parent = path.parent().context("Missing parent")?.to_owned();
        tokio::task::spawn_blocking(move || File::open(parent)?.sync_all()).await??;
    }
    Ok(())
}
fn validate_transaction(t: &Transaction, executable: &Path) -> Result<()> {
    let root = executable.parent().context("Missing runtime root")?;
    if t.target != root
        || t.directory.parent() != root.parent()
        || !t
            .directory
            .file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with(".paseo-update-"))
        || t.backup != t.directory.join("previous")
        || t.staged != t.directory.join("runtime")
        || fs::symlink_metadata(&t.directory)?.file_type().is_symlink()
    {
        bail!("Invalid native update recovery paths")
    };
    Ok(())
}
async fn rollback(t: &Transaction) -> Result<()> {
    if !tokio::fs::try_exists(&t.backup).await? {
        return verify_tree(&t.target, &t.original_digest).await;
    }
    verify_tree(&t.backup, &t.original_digest).await?;
    if tokio::fs::try_exists(&t.target).await? {
        verify_tree(&t.target, &t.new_digest).await?;
        tokio::fs::rename(&t.target, &t.staged).await?;
    }
    tokio::fs::rename(&t.backup, &t.target).await?;
    sync_parent(&t.target).await
}
async fn cleanup(home: &Path, t: &Transaction) -> Result<()> {
    tokio::fs::remove_file(home.join(RECORD)).await?;
    tokio::fs::remove_dir_all(&t.directory).await?;
    Ok(())
}
/// The supervisor retains the original executable pathname across root replacement.
/// Readiness is accepted only for the exact version/source admitted by the manifest.
pub async fn finish_restart(
    home: &Path,
    executable: &Path,
    ready: Option<(&str, &str)>,
) -> Result<bool> {
    let path = home.join(RECORD);
    let metadata = match tokio::fs::symlink_metadata(&path).await {
        Ok(v) => v,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e.into()),
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 64 * 1024 {
        bail!("Invalid native update recovery record")
    }
    let mut t: Transaction = serde_json::from_slice(&tokio::fs::read(path).await?)?;
    validate_transaction(&t, executable)?;
    let _lease = lease(&t.target)?;
    if let Some((version, source)) = ready {
        if version != t.version || source != t.source_sha {
            bail!("Updated worker provenance does not match the admitted release")
        }
        verify_tree(&t.target, &t.new_digest).await?;
        if !t.committed {
            t.committed = true;
            write_record(home, &t, true).await?;
        }
        // Windows can keep the old supervisor image mapped. Retain its recovery
        // directory until the next supervisor can remove the backup safely.
        match tokio::fs::remove_dir_all(&t.backup).await {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) if cfg!(windows) && e.kind() == std::io::ErrorKind::PermissionDenied => {
                return Ok(false);
            }
            Err(e) => return Err(e.into()),
        }
        cleanup(home, &t).await?;
        return Ok(false);
    }
    if t.committed {
        return Ok(false);
    }
    rollback(&t).await?;
    cleanup(home, &t).await?;
    Ok(true)
}

pub(super) async fn validate_binary(path: &Path) -> Result<()> {
    let (platform, arch) = platform()?;
    let mut file = tokio::fs::File::open(path).await?;
    let mut header = [0u8; 64];
    file.read_exact(&mut header).await?;
    match platform {
        "linux" => {
            let machine = u16::from_le_bytes([header[18], header[19]]);
            if &header[..4] != b"\x7fELF"
                || header[4] != 2
                || header[5] != 1
                || machine != (if arch == "x64" { 62 } else { 183 })
            {
                bail!("Native artifact ELF platform/architecture mismatch")
            }
        }
        "macos" => {
            let cpu = u32::from_le_bytes(header[4..8].try_into()?);
            if header[..4] != [0xcf, 0xfa, 0xed, 0xfe]
                || cpu
                    != (if arch == "x64" {
                        0x01000007
                    } else {
                        0x0100000c
                    })
            {
                bail!("Native artifact Mach-O platform/architecture mismatch")
            }
        }
        "windows" => {
            if &header[..2] != b"MZ" {
                bail!("Native artifact is not PE")
            };
            let offset = u32::from_le_bytes(header[60..64].try_into()?) as u64;
            file.seek(std::io::SeekFrom::Start(offset)).await?;
            let mut pe = [0u8; 26];
            file.read_exact(&mut pe).await?;
            let machine = u16::from_le_bytes([pe[4], pe[5]]);
            if &pe[..4] != b"PE\0\0"
                || machine != (if arch == "x64" { 0x8664 } else { 0xaa64 })
                || u16::from_le_bytes([pe[24], pe[25]]) != 0x20b
            {
                bail!("Native artifact PE platform/architecture mismatch")
            }
        }
        _ => unreachable!(),
    }
    Ok(())
}
#[cfg(any(target_os = "macos", windows))]
async fn native_command(program: &Path, args: &[String]) -> Result<String> {
    let mut command = tokio::process::Command::new(program);
    command
        .args(args)
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x08000000);
    let output = tokio::time::timeout(Duration::from_secs(30), command.output())
        .await
        .context("Native signature verification timed out")??;
    if !output.status.success() {
        bail!(
            "Native signature verification failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )
    };
    Ok(format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    ))
}
async fn validate_signature(old: &Path, new: &Path) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        let code = Path::new("/usr/bin/codesign");
        native_command(
            code,
            &[
                "--verify".into(),
                "--strict".into(),
                new.display().to_string(),
            ],
        )
        .await?;
        let old = native_command(
            code,
            &["-d".into(), "--verbose=4".into(), old.display().to_string()],
        )
        .await?;
        let new = native_command(
            code,
            &["-d".into(), "--verbose=4".into(), new.display().to_string()],
        )
        .await?;
        let team = |s: &str| {
            s.lines()
                .find_map(|line| line.strip_prefix("TeamIdentifier=").map(str::to_owned))
        };
        let old_team = team(&old);
        let new_team = team(&new);
        if old_team.as_deref().is_some_and(|v| v != "not set") {
            if old_team != new_team {
                bail!("Native updater signing team differs from installed executable")
            }
        } else if !old.contains("Signature=adhoc") || !new.contains("Signature=adhoc") {
            bail!("Native updater signing identity is not verifiable")
        }
    }
    #[cfg(windows)]
    {
        let quote = |path: &Path| format!("'{}'", path.to_string_lossy().replace('\'', "''"));
        let script = format!(
            "$ErrorActionPreference='Stop';$old=Get-AuthenticodeSignature -LiteralPath {};$new=Get-AuthenticodeSignature -LiteralPath {};if($new.Status -ne 'Valid' -and $new.Status -ne 'NotSigned'){{throw 'Invalid update Authenticode signature'}};if($old.Status -eq 'Valid'){{if($new.Status -ne 'Valid' -or $old.SignerCertificate.Subject -ne $new.SignerCertificate.Subject){{throw 'Update publisher mismatch'}}}}elseif($old.Status -ne 'NotSigned'){{throw 'Installed Authenticode signature is invalid'}}",
            quote(old),
            quote(new)
        );
        let bytes = script
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>();
        let root = std::env::var_os("SystemRoot")
            .context("SystemRoot is unavailable for Authenticode validation")?;
        native_command(
            &PathBuf::from(root).join("System32/WindowsPowerShell/v1.0/powershell.exe"),
            &[
                "-NoLogo".into(),
                "-NoProfile".into(),
                "-NonInteractive".into(),
                "-EncodedCommand".into(),
                STANDARD.encode(bytes),
            ],
        )
        .await?;
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let _ = (old, new);
    }
    Ok(())
}

#[cfg(test)]
mod recovery_tests {
    use super::*;
    #[tokio::test]
    async fn rollback_restores_entire_runtime_and_refuses_changed_replacement() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("installed");
        let staging = temp.path().join(".paseo-update-test");
        fs::create_dir(&root).unwrap();
        fs::create_dir(&staging).unwrap();
        fs::write(root.join("paseo"), b"old executable").unwrap();
        fs::create_dir(root.join("skills")).unwrap();
        fs::write(root.join("skills/rule"), b"old skill").unwrap();
        let staged = staging.join("runtime");
        fs::create_dir(&staged).unwrap();
        fs::write(staged.join("paseo"), b"new executable").unwrap();
        fs::create_dir(staged.join("skills")).unwrap();
        fs::write(staged.join("skills/rule"), b"new skill").unwrap();
        let t = Transaction {
            original_digest: tree_digest(&root).unwrap(),
            new_digest: tree_digest(&staged).unwrap(),
            target: root.clone(),
            staged,
            backup: staging.join("previous"),
            directory: staging,
            version: "2.0.0".into(),
            source_sha: "a".repeat(40),
            committed: false,
            deferred: false,
        };
        replace(&t).await.unwrap();
        fs::write(root.join("skills/rule"), b"unexpected change").unwrap();
        assert!(rollback(&t).await.is_err());
        assert!(t.backup.exists());
        fs::write(root.join("skills/rule"), b"new skill").unwrap();
        rollback(&t).await.unwrap();
        assert_eq!(fs::read(root.join("paseo")).unwrap(), b"old executable");
        assert_eq!(fs::read(root.join("skills/rule")).unwrap(), b"old skill");
    }
}
