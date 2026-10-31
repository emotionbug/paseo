use super::{diagnostics, updater};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::{fs, io::Write};

fn release() -> (Value, Value) {
    let artifact = "Paseo-Daemon-2.0.0-linux-x64.tar.gz";
    (
        json!({"schemaVersion":1,"runtime":"rust","appId":"sh.paseo.daemon","channel":"stable","platform":"linux","arch":"x64","version":"2.0.0","sourceSha":"a".repeat(40),"artifact":{"name":artifact,"format":"tar.gz","size":123,"sha512":STANDARD.encode([7u8;64])}}),
        json!({"tag_name":"v2.0.0","assets":[{"name":artifact,"size":123,"browser_download_url":format!("https://github.com/example/paseo/releases/download/v2.0.0/{artifact}")}]}),
    )
}
fn admitted(manifest: &Value, release: &Value) -> bool {
    updater::validate_manifest(
        manifest,
        release,
        "example/paseo",
        "stable",
        "linux",
        "x64",
        "manifest-digest".into(),
    )
    .is_ok()
}
#[test]
fn update_admission_binds_runtime_arch_version_repository_size_and_digest() {
    let (manifest, published) = release();
    assert!(admitted(&manifest, &published));
    for (key, value) in [
        ("runtime", json!("flutter")),
        ("arch", json!("arm64")),
        ("platform", json!("windows")),
        ("version", json!("3.0.0")),
        ("sourceSha", json!("not-provenance")),
    ] {
        let mut changed = manifest.clone();
        changed[key] = value;
        assert!(!admitted(&changed, &published), "{key}");
    }
    for (key, value) in [
        ("size", json!(124)),
        ("sha512", json!("bad")),
        ("name", json!("../Paseo-Daemon.tar.gz")),
        ("format", json!("binary")),
    ] {
        let mut changed = manifest.clone();
        changed["artifact"][key] = value;
        assert!(!admitted(&changed, &published), "{key}");
    }
    let mut redirected = published.clone();
    redirected["assets"][0]["browser_download_url"] = json!(
        "https://github.com/other/paseo/releases/download/v2.0.0/Paseo-Daemon-2.0.0-linux-x64.tar.gz"
    );
    assert!(!admitted(&manifest, &redirected));
    let mut duplicate = published.clone();
    let extra = duplicate["assets"][0].clone();
    duplicate["assets"].as_array_mut().unwrap().push(extra);
    assert!(!admitted(&manifest, &duplicate));
}
#[test]
fn standalone_marker_rejects_desktop_package_manager_and_mismatched_assets() {
    let marker = json!({"schemaVersion":1,"runtime":"rust","appId":"sh.paseo.daemon","installationKind":"standalone-archive","version":"2.0.0","sourceSha":"a".repeat(40),"platform":"linux","arch":"x64"});
    assert!(updater::validate_marker(&marker, "2.0.0", &"a".repeat(40), "linux", "x64").is_ok());
    for kind in ["flutter", "npm", "package-manager"] {
        let mut other = marker.clone();
        other["installationKind"] = json!(kind);
        assert!(
            updater::validate_marker(&other, "2.0.0", &"a".repeat(40), "linux", "x64").is_err()
        );
    }
    assert!(updater::validate_marker(&marker, "2.0.1", &"a".repeat(40), "linux", "x64").is_err());
}
#[test]
fn tar_runtime_rejects_symlinks_without_touching_outside_staging() {
    let temp = tempfile::tempdir().unwrap();
    let archive = temp.path().join("runtime.tar.gz");
    let encoder = flate2::write::GzEncoder::new(
        fs::File::create(&archive).unwrap(),
        flate2::Compression::default(),
    );
    let mut tar = tar::Builder::new(encoder);
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Symlink);
    header.set_mode(0o777);
    header.set_size(0);
    tar.append_link(&mut header, "escape", "../outside")
        .unwrap();
    tar.into_inner().unwrap().finish().unwrap();
    assert!(updater::extract(&archive, &temp.path().join("runtime"), "tar.gz").is_err());
    assert!(!temp.path().join("outside").exists());
}
#[test]
fn zip_runtime_rejects_traversal() {
    let temp = tempfile::tempdir().unwrap();
    let archive = temp.path().join("runtime.zip");
    let mut zip = zip::ZipWriter::new(fs::File::create(&archive).unwrap());
    zip.start_file("../outside", zip::write::SimpleFileOptions::default())
        .unwrap();
    zip.write_all(b"not outside").unwrap();
    zip.finish().unwrap();
    assert!(updater::extract(&archive, &temp.path().join("runtime"), "zip").is_err());
    assert!(!temp.path().join("outside").exists());
}
#[test]
fn archive_extraction_preserves_executable_and_runtime_assets() {
    let temp = tempfile::tempdir().unwrap();
    let archive = temp.path().join("runtime.tar.gz");
    let encoder = flate2::write::GzEncoder::new(
        fs::File::create(&archive).unwrap(),
        flate2::Compression::default(),
    );
    let mut tar = tar::Builder::new(encoder);
    for (name, content, mode) in [
        ("paseo", b"binary".as_slice(), 0o755),
        ("lib/runtime", b"library".as_slice(), 0o644),
        ("skills/rule", b"skill".as_slice(), 0o644),
    ] {
        let mut header = tar::Header::new_gnu();
        header.set_mode(mode);
        header.set_size(content.len() as u64);
        header.set_cksum();
        tar.append_data(&mut header, name, content).unwrap();
    }
    tar.into_inner().unwrap().finish().unwrap();
    let root = temp.path().join("runtime");
    updater::extract(&archive, &root, "tar.gz").unwrap();
    assert_eq!(fs::read(root.join("skills/rule")).unwrap(), b"skill");
    assert_eq!(fs::read(root.join("lib/runtime")).unwrap(), b"library");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_ne!(
            fs::metadata(root.join("paseo"))
                .unwrap()
                .permissions()
                .mode()
                & 0o111,
            0
        );
    }
}
#[tokio::test]
async fn diagnostics_tail_is_bounded_and_redacts_secrets() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("daemon.log");
    fs::write(
        &path,
        (0..200).map(|i| format!("line {i}\n")).collect::<String>(),
    )
    .unwrap();
    let tail = diagnostics::tail(&path).await.unwrap();
    assert_eq!(tail.lines().count(), 80);
    assert!(tail.contains("line 199"));
    assert!(!tail.contains("line 119"));
    let text = diagnostics::redact(
        "paseo://pairing-secret https://relay.internal/?token=abc&key=xyz {\"password\":\"hidden\"} authorization=secret",
        &["https://relay.internal/"],
    );
    for secret in [
        "pairing-secret",
        "relay.internal",
        "abc",
        "xyz",
        "hidden",
        "authorization=secret",
    ] {
        assert!(!text.contains(secret), "{secret}");
    }
}

#[test]
fn tar_runtime_rejects_duplicate_asset_paths() {
    let temp = tempfile::tempdir().unwrap();
    let archive = temp.path().join("runtime.tar.gz");
    let encoder = flate2::write::GzEncoder::new(
        fs::File::create(&archive).unwrap(),
        flate2::Compression::default(),
    );
    let mut tar = tar::Builder::new(encoder);
    for content in [b"first".as_slice(), b"second".as_slice()] {
        let mut header = tar::Header::new_gnu();
        header.set_mode(0o755);
        header.set_size(content.len() as u64);
        header.set_cksum();
        tar.append_data(&mut header, "paseo", content).unwrap();
    }
    tar.into_inner().unwrap().finish().unwrap();
    assert!(updater::extract(&archive, &temp.path().join("runtime"), "tar.gz").is_err());
}
