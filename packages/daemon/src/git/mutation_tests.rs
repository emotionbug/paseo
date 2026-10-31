use super::{command, facts, mutations};
use std::{collections::BTreeMap, path::Path};
use tokio_util::sync::CancellationToken;

async fn git(cwd: &Path, args: &[&str]) -> anyhow::Result<String> {
    command::text(cwd, args, &BTreeMap::new(), CancellationToken::new()).await
}
async fn repository() -> anyhow::Result<tempfile::TempDir> {
    let dir = tempfile::tempdir()?;
    git(dir.path(), &["init", "--initial-branch=main"]).await?;
    git(dir.path(), &["config", "user.name", "Git fixture"]).await?;
    git(
        dir.path(),
        &["config", "user.email", "fixture@example.invalid"],
    )
    .await?;
    tokio::fs::write(dir.path().join("tracked.txt"), b"before\n").await?;
    git(dir.path(), &["add", "tracked.txt"]).await?;
    git(dir.path(), &["commit", "-m", "initial"]).await?;
    Ok(dir)
}

#[tokio::test]
async fn literal_discard_does_not_interpret_pathspec_magic() -> anyhow::Result<()> {
    let dir = repository().await?;
    let special = ":(glob)*";
    tokio::fs::write(dir.path().join(special), b"literal\n").await?;
    tokio::fs::write(dir.path().join("tracked.txt"), b"keep this change\n").await?;
    mutations::discard(
        dir.path(),
        &[special.to_owned()],
        &BTreeMap::new(),
        CancellationToken::new(),
    )
    .await?;
    assert!(!dir.path().join(special).exists());
    assert_eq!(
        tokio::fs::read(dir.path().join("tracked.txt")).await?,
        b"keep this change\n"
    );
    Ok(())
}

#[tokio::test]
async fn push_uses_configured_remote_and_exact_head_ref() -> anyhow::Result<()> {
    let dir = repository().await?;
    let bare = tempfile::tempdir()?;
    git(bare.path(), &["init", "--bare"]).await?;
    git(
        dir.path(),
        &["remote", "add", "review", bare.path().to_str().unwrap()],
    )
    .await?;
    git(dir.path(), &["config", "branch.main.pushRemote", "review"]).await?;
    git(
        dir.path(),
        &[
            "config",
            "remote.review.push",
            "HEAD:refs/heads/review-target",
        ],
    )
    .await?;
    mutations::push(dir.path(), &BTreeMap::new(), CancellationToken::new()).await?;
    let expected = git(dir.path(), &["rev-parse", "HEAD"]).await?;
    assert_eq!(
        git(bare.path(), &["rev-parse", "refs/heads/review-target"]).await?,
        expected
    );
    assert!(
        !command::exists(
            bare.path(),
            "refs/heads/main",
            &BTreeMap::new(),
            CancellationToken::new()
        )
        .await?
    );
    Ok(())
}

#[tokio::test]
async fn merge_base_in_other_worktree_keeps_source_branch() -> anyhow::Result<()> {
    let dir = repository().await?;
    let worktrees = tempfile::tempdir()?;
    let feature = worktrees.path().join("feature");
    git(
        dir.path(),
        &[
            "worktree",
            "add",
            "--no-track",
            "-b",
            "feature",
            feature.to_str().unwrap(),
            "main",
        ],
    )
    .await?;
    tokio::fs::write(feature.join("tracked.txt"), b"feature\n").await?;
    git(&feature, &["commit", "-am", "feature"]).await?;
    let mutated = mutations::merge_to_base(
        &feature,
        Some("main"),
        false,
        &BTreeMap::new(),
        CancellationToken::new(),
    )
    .await?;
    assert_eq!(mutated, dir.path());
    assert_eq!(
        facts::current_branch(&feature, &BTreeMap::new(), CancellationToken::new())
            .await?
            .as_deref(),
        Some("feature")
    );
    assert_eq!(
        tokio::fs::read(dir.path().join("tracked.txt")).await?,
        b"feature\n"
    );
    Ok(())
}

#[tokio::test]
async fn mutations_outside_a_repository_report_not_git_repo() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let error = mutations::pull(dir.path(), &BTreeMap::new(), CancellationToken::new())
        .await
        .unwrap_err();
    assert!(error.downcast_ref::<command::NotGitRepo>().is_some());
    assert_eq!(
        error.to_string(),
        format!("Not a git repository: {}", dir.path().display())
    );
    Ok(())
}

#[tokio::test]
async fn merge_from_base_conflict_is_classified_and_aborted() -> anyhow::Result<()> {
    let dir = repository().await?;
    git(dir.path(), &["checkout", "-b", "feature"]).await?;
    tokio::fs::write(dir.path().join("tracked.txt"), b"feature\n").await?;
    git(dir.path(), &["commit", "-am", "feature"]).await?;
    git(dir.path(), &["checkout", "main"]).await?;
    tokio::fs::write(dir.path().join("tracked.txt"), b"main\n").await?;
    git(dir.path(), &["commit", "-am", "main"]).await?;
    git(dir.path(), &["checkout", "feature"]).await?;
    let error = mutations::merge_from_base(
        dir.path(),
        Some("main"),
        true,
        &BTreeMap::new(),
        CancellationToken::new(),
    )
    .await
    .unwrap_err();
    assert!(error.downcast_ref::<mutations::MergeConflict>().is_some());
    assert_eq!(
        error.to_string(),
        "Merge conflict while merging main into feature. Please merge manually."
    );
    assert!(!dir.path().join(".git/MERGE_HEAD").exists());
    assert_eq!(
        tokio::fs::read(dir.path().join("tracked.txt")).await?,
        b"feature\n"
    );
    Ok(())
}
