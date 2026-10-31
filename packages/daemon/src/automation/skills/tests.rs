use super::{
    files::{self, Op, Targets},
    transaction::Transaction,
};
use anyhow::Result;
use serde_json::json;
use std::fs;

fn fixture() -> Result<(tempfile::TempDir, Targets)> {
    let temporary = tempfile::tempdir()?;
    let targets = Targets {
        source: temporary.path().join("bundle"),
        roots: [
            temporary.path().join("agents/skills"),
            temporary.path().join("claude/skills"),
            temporary.path().join("codex/skills"),
        ],
    };
    fs::create_dir_all(targets.source.join("paseo/references"))?;
    fs::write(targets.source.join("paseo/SKILL.md"), "# Paseo fixture\n")?;
    fs::write(
        targets.source.join("paseo/references/guide.md"),
        "Reference fixture\n",
    )?;
    Ok((temporary, targets))
}

fn install(targets: &Targets) -> Result<()> {
    let selection = json!({"mode":"all"});
    let plan = files::status(targets, &selection)?;
    assert!(files::apply(targets, &selection, &plan.ops)?.ops.is_empty());
    Ok(())
}

#[test]
fn catalog_drift_does_not_count_unrelated_or_extra_user_files() -> Result<()> {
    let (_temporary, targets) = fixture()?;
    let all = json!({"mode":"all"});
    let initial = files::status(&targets, &all)?;
    assert_eq!(initial.state, "not-installed");
    assert_eq!(initial.available, ["paseo"]);
    assert_eq!(initial.ops[0].kind, "add");
    install(&targets)?;
    fs::write(targets.roots[0].join("paseo/notes.md"), "User-owned notes")?;
    fs::create_dir_all(targets.roots[0].join("user-skill"))?;
    fs::write(targets.roots[0].join("user-skill/SKILL.md"), "User skill")?;
    assert_eq!(files::status(&targets, &all)?.state, "up-to-date");
    fs::write(
        targets.roots[1].join("paseo/SKILL.md"),
        "Changed bundle file",
    )?;
    let drift = files::status(&targets, &all)?;
    assert_eq!(drift.state, "drift");
    assert_eq!(drift.ops[0].kind, "update");
    Ok(())
}

#[test]
fn selection_normalizes_names_without_treating_unknown_names_as_paths() -> Result<()> {
    let (_temporary, targets) = fixture()?;
    let selection = files::selection(
        &json!({"mode":"custom","skills":[" paseo ","paseo","../escape",null,""]}),
    );
    assert_eq!(
        selection,
        json!({"mode":"custom","skills":["../escape","paseo"]})
    );
    let status = files::status(&targets, &selection)?;
    assert_eq!(status.ops.len(), 1);
    assert_eq!(status.ops[0].name, "paseo");
    Ok(())
}

#[test]
fn stale_manifest_files_are_pruned_only_when_unmodified() -> Result<()> {
    let (_temporary, targets) = fixture()?;
    install(&targets)?;
    fs::remove_file(targets.source.join("paseo/references/guide.md"))?;
    fs::write(
        targets.roots[0].join("paseo/references/guide.md"),
        "User-edited reference",
    )?;
    let ops = [Op {
        kind: "update".into(),
        name: "paseo".into(),
    }];
    files::apply(&targets, &json!({"mode":"all"}), &ops)?;
    assert_eq!(
        fs::read_to_string(targets.roots[0].join("paseo/references/guide.md"))?,
        "User-edited reference"
    );
    assert!(!targets.roots[1].join("paseo/references/guide.md").exists());
    Ok(())
}

#[test]
fn rollback_merges_external_files_and_quarantines_conflicting_originals() -> Result<()> {
    let (_temporary, targets) = fixture()?;
    install(&targets)?;
    let previous = json!({"mode":"all"});
    let next = json!({"mode":"custom","skills":[]});
    let ops = files::status(&targets, &next)?.ops;
    let transaction = Transaction::begin(&targets, previous, next, &ops)?;
    let live = targets.roots[0].join("paseo");
    fs::create_dir_all(&live)?;
    fs::write(live.join("SKILL.md"), "Concurrent replacement")?;
    fs::write(live.join("notes.md"), "Concurrent notes")?;
    transaction.rollback(&targets)?;
    assert_eq!(
        fs::read_to_string(live.join("SKILL.md"))?,
        "Concurrent replacement"
    );
    assert_eq!(
        fs::read_to_string(live.join("notes.md"))?,
        "Concurrent notes"
    );
    assert!(live.join("references/guide.md").is_file());
    let parent = &targets.roots[0];
    let recovered = fs::read_dir(parent)?
        .collect::<std::io::Result<Vec<_>>>()?
        .into_iter()
        .find(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with(".paseo-skills-recovered-paseo-")
        });
    assert!(recovered.is_some());
    assert_eq!(
        fs::read_to_string(recovered.unwrap().path().join("SKILL.md"))?,
        "# Paseo fixture\n"
    );
    Ok(())
}

#[test]
fn recovery_distinguishes_committed_and_uncommitted_selection() -> Result<()> {
    let (_temporary, targets) = fixture()?;
    install(&targets)?;
    let previous = json!({"mode":"all"});
    let next = json!({"mode":"custom","skills":[]});
    let ops = files::status(&targets, &next)?.ops;
    let _interrupted = Transaction::begin(&targets, previous.clone(), next.clone(), &ops)?;
    super::transaction::recover(&targets, &previous)?;
    assert_eq!(files::status(&targets, &previous)?.state, "up-to-date");
    let _committed = Transaction::begin(&targets, previous, next.clone(), &ops)?;
    super::transaction::recover(&targets, &next)?;
    assert_eq!(files::status(&targets, &next)?.state, "not-installed");
    Ok(())
}

#[test]
fn recovery_leaves_unowned_and_out_of_bounds_journals_untouched() -> Result<()> {
    let (temporary, targets) = fixture()?;
    let parent = targets.roots[0].parent().unwrap();
    let unrelated = parent.join(".paseo-skills-transaction-notes");
    fs::create_dir_all(&unrelated)?;
    fs::write(unrelated.join("notes.txt"), "Keep me")?;
    let forged = parent.join(".paseo-skills-transaction-forged");
    fs::create_dir_all(&forged)?;
    fs::write(
        forged.join("transaction.json"),
        serde_json::to_vec(&json!({
            "owner":"paseo-skills-transaction","version":1,"phase":"converging",
            "previousSelection":{"mode":"all"},"nextSelection":{"mode":"custom","skills":[]},
            "entries":[{"livePath":temporary.path().join("outside"),"kind":"delete","backupPath":null}]
        }))?,
    )?;
    super::transaction::recover(&targets, &json!({"mode":"all"}))?;
    assert!(unrelated.join("notes.txt").is_file());
    assert!(forged.join("transaction.json").is_file());
    Ok(())
}

#[cfg(unix)]
#[test]
fn managed_file_symlinks_are_rejected_without_touching_their_target() -> Result<()> {
    let (temporary, targets) = fixture()?;
    install(&targets)?;
    let external = temporary.path().join("external.md");
    fs::write(&external, "External content")?;
    let managed = targets.roots[0].join("paseo/SKILL.md");
    fs::remove_file(&managed)?;
    std::os::unix::fs::symlink(&external, managed)?;
    let error = files::apply(
        &targets,
        &json!({"mode":"all"}),
        &[Op {
            kind: "update".into(),
            name: "paseo".into(),
        }],
    )
    .unwrap_err();
    assert!(error.to_string().contains("symbolic link"));
    assert_eq!(fs::read_to_string(external)?, "External content");
    Ok(())
}

#[cfg(unix)]
#[test]
fn aliased_roots_stage_once_and_restore_relative_skill_symlinks() -> Result<()> {
    let (temporary, targets) = fixture()?;
    install(&targets)?;
    fs::remove_dir_all(&targets.roots[1])?;
    std::os::unix::fs::symlink(&targets.roots[0], &targets.roots[1])?;
    let shared = temporary.path().join("shared");
    fs::rename(targets.roots[0].join("paseo"), &shared)?;
    std::os::unix::fs::symlink("../../shared", targets.roots[0].join("paseo"))?;
    let previous = json!({"mode":"all"});
    let next = json!({"mode":"custom","skills":[]});
    let ops = files::status(&targets, &next)?.ops;
    let transaction = Transaction::begin(&targets, previous, next, &ops)?;
    transaction.rollback(&targets)?;
    assert_eq!(
        fs::read_link(targets.roots[0].join("paseo"))?,
        std::path::PathBuf::from("../../shared")
    );
    assert!(targets.roots[1].join("paseo/SKILL.md").is_file());
    Ok(())
}
