use super::*;

struct Repo {
    directory: tempfile::TempDir,
    env: BTreeMap<String, String>,
}
impl Repo {
    async fn new() -> Result<Self> {
        let directory = tempfile::tempdir()?;
        let mut env = BTreeMap::new();
        env.insert("GIT_CONFIG_NOSYSTEM".into(), "1".into());
        env.insert("GIT_CONFIG_GLOBAL".into(), null_device().into());
        env.insert(
            "HOME".into(),
            directory.path().to_string_lossy().into_owned(),
        );
        env.insert("GIT_AUTHOR_NAME".into(), "Fixture".into());
        env.insert("GIT_AUTHOR_EMAIL".into(), "fixture@example.invalid".into());
        env.insert("GIT_COMMITTER_NAME".into(), "Fixture".into());
        env.insert(
            "GIT_COMMITTER_EMAIL".into(),
            "fixture@example.invalid".into(),
        );
        let repo = Self { directory, env };
        repo.git(&["init", "-b", "main"]).await?;
        Ok(repo)
    }
    fn path(&self) -> &Path {
        self.directory.path()
    }
    async fn git(&self, values: &[&str]) -> Result<Vec<u8>> {
        git(
            self.path(),
            &args(values),
            &self.env,
            &CancellationToken::new(),
            INVENTORY_LIMIT,
        )
        .await
    }
    async fn write(&self, path: &str, bytes: &[u8]) -> Result<()> {
        tokio::fs::write(self.path().join(path), bytes).await?;
        Ok(())
    }
    async fn commit(&self, message: &str) -> Result<String> {
        self.git(&["add", "-A"]).await?;
        self.git(&["-c", "commit.gpgsign=false", "commit", "-m", message])
            .await?;
        Ok(String::from_utf8(self.git(&["rev-parse", "HEAD"]).await?)?
            .trim()
            .to_owned())
    }
    async fn diff(&self, compare: Value) -> Result<Value> {
        read(self.path(), &compare, &self.env, CancellationToken::new()).await
    }
}

#[tokio::test]
async fn unborn_summary_full_binary_and_whitespace() -> Result<()> {
    let repo = Repo::new().await?;
    repo.write("new.txt", b"first\nlast").await?;
    repo.write("binary.dat", b"a\0b").await?;
    let summary = repo
        .diff(json!({"mode":"uncommitted","detail":"summary"}))
        .await?;
    assert_eq!(summary["error"], Value::Null);
    assert_eq!(summary["files"][0]["status"], "binary");
    assert_eq!(summary["files"][1]["additions"], 2);
    assert_eq!(summary["files"][1]["hunks"], json!([]));
    let full = repo.diff(json!({"mode":"uncommitted"})).await?;
    assert_eq!(full["files"][1]["hunks"][0]["lines"][2]["content"], "last");
    repo.commit("initial").await?;
    repo.write("new.txt", b" first\nlast   ").await?;
    assert_eq!(
        repo.diff(json!({"mode":"uncommitted","ignoreWhitespace":true}))
            .await?["files"],
        json!([])
    );
    Ok(())
}

#[tokio::test]
async fn root_commit_rename_and_binary_file_reads() -> Result<()> {
    let repo = Repo::new().await?;
    let old = "old file\tname.txt";
    let new = "new file\nname.txt";
    repo.write(old, b"one\ntwo\nthree\nfour\n").await?;
    repo.write("binary.dat", b"\0binary").await?;
    let root = repo.commit("root").await?;
    let entry = commit_file(repo.path(), &root, old, &repo.env, CancellationToken::new())
        .await?
        .unwrap();
    assert_eq!(entry["isNew"], true);
    assert_eq!(entry["additions"], 4);
    assert!(
        commit_file(
            repo.path(),
            &root,
            "binary.dat",
            &repo.env,
            CancellationToken::new()
        )
        .await?
        .is_none()
    );
    repo.git(&["mv", old, new]).await?;
    let rename = repo.commit("rename").await?;
    let entry = commit_file(
        repo.path(),
        &rename,
        new,
        &repo.env,
        CancellationToken::new(),
    )
    .await?
    .unwrap();
    assert_eq!(entry["oldPath"], old);
    assert_eq!(entry["path"], new);
    assert!(!entry["isNew"].as_bool().unwrap());
    assert!(
        commit_file(
            repo.path(),
            &rename,
            "absent",
            &repo.env,
            CancellationToken::new()
        )
        .await?
        .is_none()
    );
    let history = list_commits(repo.path(), &repo.env, CancellationToken::new()).await?;
    assert_eq!(history["commits"][0]["files"][0]["status"], "renamed");
    assert_eq!(history["commits"][0]["isOnRemote"], false);
    Ok(())
}

#[tokio::test]
async fn all_comparison_restores_only_private_index() -> Result<()> {
    let repo = Repo::new().await?;
    repo.write("restored.txt", b"original\n").await?;
    repo.commit("base").await?;
    repo.git(&["checkout", "-b", "feature"]).await?;
    repo.git(&["rm", "restored.txt"]).await?;
    repo.commit("delete").await?;
    repo.write("restored.txt", b"changed\n").await?;
    let index = repo.path().join(".git/index");
    let before = tokio::fs::read(&index).await?;
    let result = repo.diff(json!({"mode":"all","baseRef":"main"})).await?;
    assert_eq!(result["error"], Value::Null);
    assert_eq!(result["files"].as_array().unwrap().len(), 1);
    assert_eq!(result["files"][0]["isNew"], false);
    assert_eq!(result["files"][0]["additions"], 1);
    assert_eq!(result["files"][0]["deletions"], 1);
    assert_eq!(tokio::fs::read(&index).await?, before);
    Ok(())
}

#[tokio::test]
async fn budgets_keep_small_neighbors_and_real_numstat() -> Result<()> {
    let repo = Repo::new().await?;
    repo.write("a-huge", b"old\n").await?;
    repo.write("z-small", b"old\n").await?;
    repo.commit("base").await?;
    repo.write("a-huge", &vec![b'x'; FILE_LIMIT + 1]).await?;
    repo.write("z-small", b"new\n").await?;
    let result = repo.diff(json!({"mode":"uncommitted"})).await?;
    assert_eq!(result["error"], Value::Null);
    assert_eq!(result["files"][0]["status"], "too_large");
    assert_eq!(result["files"][0]["additions"], 1);
    assert_eq!(result["files"][1]["status"], "ok");
    assert_eq!(result["files"][1]["hunks"][0]["lines"][2]["content"], "new");
    Ok(())
}

#[tokio::test]
async fn merge_commit_file_uses_first_parent() -> Result<()> {
    let repo = Repo::new().await?;
    repo.write("base", b"base\n").await?;
    repo.commit("base").await?;
    repo.git(&["checkout", "-b", "side"]).await?;
    repo.write("side", b"side\n").await?;
    repo.commit("side").await?;
    repo.git(&["checkout", "main"]).await?;
    repo.write("main", b"main\n").await?;
    repo.commit("main").await?;
    repo.git(&[
        "-c",
        "commit.gpgsign=false",
        "merge",
        "--no-ff",
        "-m",
        "merge",
        "side",
    ])
    .await?;
    let entry = commit_file(
        repo.path(),
        "HEAD",
        "side",
        &repo.env,
        CancellationToken::new(),
    )
    .await?
    .unwrap();
    assert_eq!(entry["isNew"], true);
    assert_eq!(entry["additions"], 1);
    assert!(
        commit_file(
            repo.path(),
            "HEAD",
            "main",
            &repo.env,
            CancellationToken::new()
        )
        .await?
        .is_none()
    );
    Ok(())
}

#[test]
fn nul_records_preserve_rename_and_tab_newline_paths() -> Result<()> {
    let changes = parse::changes(b"R100\0old\tname\0new\nname\0M\0ordinary\0")?;
    assert_eq!(changes[0].path, "new\nname");
    assert_eq!(changes[0].old_path.as_deref(), Some("old\tname"));
    let stats = parse::stats(b"1\t2\t\0old\tname\0new\nname\0-\t-\tbinary\0")?;
    assert_eq!(stats["new\nname"].deletions, 2);
    assert!(stats["binary"].binary);
    Ok(())
}

#[tokio::test]
async fn aggregate_budget_keeps_later_small_files_and_cancellation_propagates() -> Result<()> {
    let repo = Repo::new().await?;
    for name in ["a", "b", "c"] {
        repo.write(name, &vec![b'x'; 750_000]).await?;
    }
    repo.write("z", b"small\n").await?;
    let result = repo.diff(json!({"mode":"uncommitted"})).await?;
    assert_eq!(result["error"], Value::Null);
    assert_eq!(result["files"][0]["status"], "ok");
    assert_eq!(result["files"][1]["status"], "ok");
    assert_eq!(result["files"][2]["status"], "too_large");
    assert_eq!(result["files"][3]["status"], "ok");
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert!(
        read(
            repo.path(),
            &json!({"mode":"uncommitted"}),
            &repo.env,
            cancel
        )
        .await
        .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn shortstat_requires_comparison_and_counts_untracked_text() -> Result<()> {
    let repo = Repo::new().await?;
    repo.write("tracked", b"old\n").await?;
    repo.commit("base").await?;
    repo.write("tracked", b"new\nmore\n").await?;
    repo.write("untracked", b"one\ntwo").await?;
    repo.write("binary", b"\0binary").await?;
    assert!(
        shortstat(repo.path(), &repo.env, CancellationToken::new())
            .await?
            .is_none()
    );
    repo.git(&["update-ref", "refs/remotes/origin/main", "HEAD"])
        .await?;
    assert_eq!(
        shortstat(repo.path(), &repo.env, CancellationToken::new()).await?,
        Some(json!({"additions":4,"deletions":1}))
    );
    repo.commit("changes").await?;
    repo.git(&["update-ref", "refs/remotes/origin/main", "HEAD"])
        .await?;
    assert!(
        shortstat(repo.path(), &repo.env, CancellationToken::new())
            .await?
            .is_none()
    );
    Ok(())
}
