use super::*;

#[test]
fn source_references_preserve_transport_colons_and_registry_identity() {
    assert_eq!(
        reference::split("git@host:repository.git"),
        ("git@host:repository.git".into(), None)
    );
    assert_eq!(
        reference::split("git@host:repository.git:plugins/example"),
        (
            "git@host:repository.git".into(),
            Some("plugins/example".into())
        )
    );
    assert_eq!(
        reference::split("https://host:443/repository.git:plugins/example"),
        (
            "https://host:443/repository.git".into(),
            Some("plugins/example".into())
        )
    );
    assert_eq!(
        reference::split("npm:@scope/package@1.2.3:plugin"),
        ("npm:@scope/package@1.2.3".into(), Some("plugin".into()))
    );
    assert_eq!(
        reference::split("C:\\plugins\\example"),
        ("C:\\plugins\\example".into(), None)
    );
    assert_eq!(
        reference::registry("acme/example", "https://plugins.example/internal/").unwrap(),
        Some(json!({"url":"https://plugins.example/internal","id":"acme/example"}))
    );
    assert!(
        reference::registry("github:acme/example", "https://plugins.example")
            .unwrap()
            .is_none()
    );
    assert_eq!(
        split_source_reference("registry.example:8443/acme/example").unwrap(),
        ("registry.example:8443/acme/example".into(), None)
    );
    assert!(plugin_path(Some("../escape")).is_err());
    assert!(plugin_path(Some("nested/../../escape")).is_err());
}

#[tokio::test]
async fn corrupt_registry_is_not_silently_replaced() {
    let home = tempfile::tempdir().unwrap();
    tokio::fs::create_dir(home.path().join("plugins"))
        .await
        .unwrap();
    let path = home.path().join("plugins/sources.json");
    tokio::fs::write(&path, b"{broken").await.unwrap();
    let sources = ManagedSources::new(home.path().into(), BTreeMap::new());
    assert!(sources.initialize().await.is_err());
    assert_eq!(tokio::fs::read(&path).await.unwrap(), b"{broken");
}

#[tokio::test]
async fn registry_persists_before_publishing_and_survives_restart() {
    let home = tempfile::tempdir().unwrap();
    let sources = ManagedSources::new(home.path().into(), BTreeMap::new());
    sources.initialize().await.unwrap();
    let record = json!({"remote":"file:///repository","revision":"obsolete","root":"obsolete"});
    sources.commit("example", &record).await.unwrap();
    let restarted = ManagedSources::new(home.path().into(), BTreeMap::new());
    restarted.initialize().await.unwrap();
    assert_eq!(
        restarted.record("example").await.unwrap(),
        Some(json!({"kind":"git","remote":"file:///repository"}))
    );
    tokio::fs::remove_file(home.path().join("plugins/sources.json"))
        .await
        .unwrap();
    tokio::fs::create_dir(home.path().join("plugins/sources.json"))
        .await
        .unwrap();
    assert!(
        sources
            .commit("second", &json!({"kind":"npm"}))
            .await
            .is_err()
    );
    assert!(sources.record("second").await.unwrap().is_none());
    assert!(sources.record("example").await.unwrap().is_some());
}

#[tokio::test]
async fn ownership_cannot_be_conferred_on_other_plugins_or_local_directories() {
    let home = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let sources = ManagedSources::new(home.path().into(), BTreeMap::new());
    sources.initialize().await.unwrap();
    sources
        .commit(
            "example",
            &json!({"kind":"git","remote":"file:///repository"}),
        )
        .await
        .unwrap();
    let other = sources
        .root
        .join("other")
        .join(Uuid::new_v4().to_string())
        .join("checkout");
    tokio::fs::create_dir_all(&other).await.unwrap();
    assert!(sources.remove_version("example", &other).await.is_err());
    assert!(tokio::fs::metadata(&other).await.unwrap().is_dir());
    assert!(
        sources
            .remove_version("example", outside.path())
            .await
            .is_err()
    );
    assert!(tokio::fs::metadata(outside.path()).await.unwrap().is_dir());
}

#[tokio::test]
async fn npm_revision_and_proposal_are_bound_to_acquisition_artifacts() {
    let home = tempfile::tempdir().unwrap();
    let sources = ManagedSources::new(home.path().into(), BTreeMap::new());
    sources.initialize().await.unwrap();
    let version = sources
        .root
        .join("example")
        .join(Uuid::new_v4().to_string());
    let package = version.join("node_modules/fixture");
    tokio::fs::create_dir_all(&package).await.unwrap();
    tokio::fs::write(
        version.join("package.json"),
        br#"{"dependencies":{"fixture":"1.2.3"}}"#,
    )
    .await
    .unwrap();
    tokio::fs::write(version.join("package-lock.json"),br#"{"lockfileVersion":3,"packages":{"node_modules/fixture":{"version":"1.2.3","resolved":"https://registry.example/fixture.tgz","integrity":"sha512-fixture"}}}"#).await.unwrap();
    tokio::fs::write(
        package.join("package.json"),
        br#"{"name":"fixture","version":"1.2.3"}"#,
    )
    .await
    .unwrap();
    sources
        .commit("example", &json!({"kind":"npm"}))
        .await
        .unwrap();
    let current = sources.describe("example", &package).await.unwrap();
    assert_eq!(current["currentRevision"], "1.2.3");
    let proposal = json!({"id":"example","expected":{"identity":current["identity"],"installationRoot":version,"revision":"1.2.3"},"target":{"kind":"npm","version":"1.2.4","resolved":"https://registry.example/next.tgz","integrity":"sha512-next"}});
    sources.assert_current(&proposal, &package).await.unwrap();
    tokio::fs::write(
        package.join("package.json"),
        br#"{"name":"fixture","version":"9.0.0"}"#,
    )
    .await
    .unwrap();
    assert!(
        sources
            .describe("example", &package)
            .await
            .unwrap()
            .get("currentRevision")
            .is_none()
    );
    assert!(sources.assert_current(&proposal, &package).await.is_err());
}

#[cfg(unix)]
#[tokio::test]
async fn symlinked_plugin_subdirectories_cannot_escape_acquisition_root() {
    let home = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let root = home.path().join("checkout");
    tokio::fs::create_dir(&root).await.unwrap();
    std::os::unix::fs::symlink(outside.path(), root.join("plugin")).unwrap();
    assert!(containment(&root, &root.join("plugin")).await.is_err());
}

#[tokio::test]
async fn git_updates_install_reviewed_commit_and_reject_replaced_generation() {
    let fixture = tempfile::tempdir().unwrap();
    let repository = fixture.path().join("repository");
    tokio::fs::create_dir(&repository).await.unwrap();
    let global = fixture.path().join("gitconfig");
    tokio::fs::write(&global, b"").await.unwrap();
    let mut env: BTreeMap<String, String> = std::env::vars()
        .filter(|(key, _)| {
            matches!(
                key.as_str(),
                "PATH" | "PATHEXT" | "SYSTEMROOT" | "SystemRoot" | "TEMP" | "TMP"
            )
        })
        .collect();
    env.insert("HOME".into(), fixture.path().to_string_lossy().into_owned());
    env.insert("GIT_CONFIG_NOSYSTEM".into(), "1".into());
    env.insert(
        "GIT_CONFIG_GLOBAL".into(),
        global.to_string_lossy().into_owned(),
    );
    env.insert("GIT_AUTHOR_NAME".into(), "Plugin fixture".into());
    env.insert("GIT_AUTHOR_EMAIL".into(), "fixture@example.invalid".into());
    env.insert("GIT_COMMITTER_NAME".into(), "Plugin fixture".into());
    env.insert(
        "GIT_COMMITTER_EMAIL".into(),
        "fixture@example.invalid".into(),
    );
    let run_git = |args: &[&str]| {
        command(
            "git",
            args.iter().map(|s| (*s).to_owned()).collect(),
            &repository,
            env.clone(),
        )
    };
    run_git(&["init", "-b", "main"]).await.unwrap();
    tokio::fs::write(repository.join("paseo-plugin.json"), br#"{"id":"fixture"}"#)
        .await
        .unwrap();
    run_git(&["add", "."]).await.unwrap();
    run_git(&["commit", "-m", "initial"]).await.unwrap();
    let sources = ManagedSources::new(fixture.path().join("daemon"), env.clone());
    sources.initialize().await.unwrap();
    let remote = url::Url::from_directory_path(&repository)
        .unwrap()
        .to_string();
    let installed = sources
        .place(
            "fixture",
            sources
                .prepare_install(&json!({"source":remote}))
                .await
                .unwrap(),
        )
        .await
        .unwrap();
    sources.verify("fixture", &installed, None).await.unwrap();
    sources.commit("fixture", &installed.record).await.unwrap();
    tokio::fs::write(repository.join("content.txt"), b"reviewed")
        .await
        .unwrap();
    run_git(&["add", "."]).await.unwrap();
    run_git(&["commit", "-m", "reviewed"]).await.unwrap();
    let preview = sources
        .preview("fixture", &installed.directory, None)
        .await
        .unwrap();
    assert_eq!(preview["outcome"], "update");
    let reviewed = preview["target"]["commit"].as_str().unwrap().to_owned();
    tokio::fs::write(repository.join("content.txt"), b"newer-unreviewed")
        .await
        .unwrap();
    run_git(&["add", "."]).await.unwrap();
    run_git(&["commit", "-m", "unreviewed"]).await.unwrap();
    let update = sources
        .prepare_update(&preview["proposal"], &installed.directory)
        .await
        .unwrap();
    let update = sources.place("fixture", update).await.unwrap();
    sources.verify("fixture", &update, None).await.unwrap();
    assert_eq!(
        git::rev_parse(&update.directory, "HEAD", &env)
            .await
            .unwrap(),
        reviewed
    );
    assert_eq!(
        tokio::fs::read(update.directory.join("content.txt"))
            .await
            .unwrap(),
        b"reviewed"
    );
    assert!(
        sources
            .assert_current(&preview["proposal"], &update.directory)
            .await
            .is_err()
    );
    sources.discard(&update).await.unwrap();
    assert!(tokio::fs::metadata(&update.directory).await.is_err());
    assert!(
        tokio::fs::metadata(&installed.directory)
            .await
            .unwrap()
            .is_dir()
    );
    sources.remove("fixture").await.unwrap();
    assert!(sources.record("fixture").await.unwrap().is_none());
    assert!(tokio::fs::metadata(&installed.directory).await.is_err());
}
