use super::super::error;
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    path::{Component, Path},
};

async fn safe_metadata(root: &Path, relative: &str) -> Result<std::fs::Metadata> {
    let mut current = root.to_owned();
    let mut metadata = None;
    for part in Path::new(relative).components() {
        let Component::Normal(part) = part else {
            return Err(error(
                "HUB_BUNDLE_UNSAFE_PATH",
                "Bundle paths must stay inside the project",
            ));
        };
        current.push(part);
        let item = tokio::fs::symlink_metadata(&current).await.map_err(|e| {
            error(
                if e.kind() == std::io::ErrorKind::NotFound {
                    "HUB_BUNDLE_FILE_MISSING"
                } else {
                    "HUB_BUNDLE_UNREADABLE"
                },
                format!("Could not read {relative}. Check the file and permissions."),
            )
        })?;
        ensure!(
            !item.file_type().is_symlink(),
            error(
                "HUB_BUNDLE_UNSAFE_PATH",
                format!("{relative} must not use a symlink.")
            )
        );
        metadata = Some(item);
    }
    metadata.ok_or_else(|| error("HUB_BUNDLE_UNSAFE_PATH", "Bundle path is empty"))
}
async fn file(root: &Path, relative: &str) -> Result<String> {
    let metadata = safe_metadata(root, relative).await?;
    ensure!(
        metadata.is_file(),
        error(
            "HUB_BUNDLE_NOT_FILE",
            format!("{relative} must be a regular file.")
        )
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        ensure!(
            metadata.permissions().mode() & 0o444 != 0,
            error(
                "HUB_BUNDLE_UNREADABLE",
                format!("Could not read {relative}. Check the file and permissions.")
            )
        );
    }
    let bytes = tokio::fs::read(root.join(relative)).await.map_err(|_| {
        error(
            "HUB_BUNDLE_UNREADABLE",
            format!("Could not read {relative}. Check the file and permissions."),
        )
    })?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}
async fn children(root: &Path, relative: &str) -> Result<Vec<(String, std::fs::FileType)>> {
    ensure!(
        safe_metadata(root, relative).await?.is_dir(),
        error(
            "HUB_WORKFLOW_DIRECTORY_INVALID",
            format!("{relative} must be a directory.")
        )
    );
    let mut directory = tokio::fs::read_dir(root.join(relative)).await?;
    let mut entries = Vec::new();
    while let Some(entry) = directory.next_entry().await? {
        entries.push((
            entry.file_name().to_string_lossy().into_owned(),
            entry.file_type().await?,
        ));
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(entries)
}
pub(super) async fn triggers(root: &Path) -> Result<Vec<Value>> {
    match tokio::fs::symlink_metadata(root.join(".paseo/triggers")).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            match tokio::fs::symlink_metadata(root.join(".paseo/hub.yml")).await {
                Ok(_) => {
                    return Err(error(
                        "HUB_PROJECT_REQUIRED",
                        "This directory contains a legacy .paseo/hub.yml bundle. Pass --project <slug> to deploy it.",
                    ));
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
            return Err(error(
                "HUB_TRIGGER_DIRECTORY_MISSING",
                ".paseo/triggers does not exist. Run this command from the project root.",
            ));
        }
        Err(e) => return Err(e.into()),
    }
    let mut triggers = Vec::new();
    for (name, kind) in children(root, ".paseo/triggers").await? {
        let path = format!(".paseo/triggers/{name}");
        ensure!(
            !kind.is_symlink(),
            error(
                "HUB_TRIGGER_UNSAFE_PATH",
                format!("{path} must not use a symlink.")
            )
        );
        ensure!(
            kind.is_file() && name.ends_with(".yml"),
            error(
                "HUB_TRIGGER_PATH_UNSUPPORTED",
                format!("{path} must be a direct-child .yml trigger.")
            )
        );
        triggers.push(json!({"path":path,"yaml":file(root,&path).await?}));
    }
    ensure!(
        !triggers.is_empty(),
        error(
            "HUB_TRIGGER_MISSING",
            ".paseo/triggers must contain at least one direct-child .yml trigger."
        )
    );
    Ok(triggers)
}
pub(super) async fn project(root: &Path, slug: &str) -> Result<(Value, usize)> {
    ensure!(
        !slug.is_empty()
            && slug.split('-').all(|part| !part.is_empty()
                && part
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())),
        error(
            "HUB_INVALID_PROJECT",
            "Project must be a bare slug such as my-project."
        )
    );
    match tokio::fs::symlink_metadata(root.join(".paseo/hub.toml")).await {
        Ok(_) => {
            return Err(error(
                "HUB_CONFIGURATION_EXTENSION_UNSUPPORTED",
                ".paseo/hub.toml is not supported. Use .paseo/hub.yml.",
            ));
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    let mut files =
        vec![json!({"path":".paseo/hub.yml","content":file(root,".paseo/hub.yml").await?})];
    let mut includes = BTreeSet::new();
    let mut count = 0;
    for (name, kind) in children(root, ".paseo/workflows").await? {
        let path = format!(".paseo/workflows/{name}");
        ensure!(
            !kind.is_symlink(),
            error(
                "HUB_BUNDLE_UNSAFE_PATH",
                format!("{path} must not use a symlink.")
            )
        );
        if kind.is_dir() && name == "partials" {
            continue;
        }
        ensure!(
            !kind.is_dir(),
            error(
                "HUB_WORKFLOW_PATH_UNSUPPORTED",
                format!("{path} is not a direct-child .yml workflow.")
            )
        );
        ensure!(
            kind.is_file(),
            error(
                "HUB_BUNDLE_NOT_FILE",
                format!("{path} must be a regular file.")
            )
        );
        ensure!(
            name.ends_with(".yml"),
            error(
                "HUB_WORKFLOW_EXTENSION_UNSUPPORTED",
                format!("{path} must use the .yml extension.")
            )
        );
        let content = file(root, &path).await?;
        let document: Value = serde_yaml::from_str(&content).map_err(|_| {
            error(
                "HUB_WORKFLOW_INVALID_YAML",
                format!("{path} is not valid YAML."),
            )
        })?;
        if let Some(steps) = document["steps"].as_array() {
            for step in steps {
                if let Some(prompt) = step["prompt"].as_array() {
                    for block in prompt {
                        if let Some(include) = block.get("include") {
                            let include = include.as_str().ok_or_else(|| {
                                error(
                                    "HUB_PARTIAL_PATH_INVALID",
                                    format!("{path} includes an invalid partial path"),
                                )
                            })?;
                            includes.insert(partial_path(&path, include)?);
                        }
                    }
                }
            }
        }
        files.push(json!({"path":path,"content":content}));
        count += 1;
    }
    ensure!(
        count > 0,
        error(
            "HUB_WORKFLOW_MISSING",
            ".paseo/workflows must contain at least one direct-child .yml workflow."
        )
    );
    for path in includes {
        files.push(json!({"path":path,"content":file(root,&path).await?}));
    }
    files.sort_by(|a, b| a["path"].as_str().cmp(&b["path"].as_str()));
    Ok((json!({"projectSlug":slug,"files":files}), count))
}
fn partial_path(workflow: &str, input: &str) -> Result<String> {
    let invalid = || {
        error(
            "HUB_PARTIAL_PATH_INVALID",
            format!(
                "{workflow} includes an unsafe path outside .paseo/workflows/partials: {input}"
            ),
        )
    };
    let mut decoded = input.to_owned();
    for _ in 0..8 {
        let mut bytes = Vec::with_capacity(decoded.len());
        let mut i = 0;
        let source = decoded.as_bytes();
        while i < source.len() {
            if source[i] == b'%' {
                if i + 2 >= source.len() {
                    return Err(invalid());
                }
                let hi = (source[i + 1] as char).to_digit(16).ok_or_else(&invalid)?;
                let lo = (source[i + 2] as char).to_digit(16).ok_or_else(&invalid)?;
                bytes.push((hi * 16 + lo) as u8);
                i += 3;
            } else {
                bytes.push(source[i]);
                i += 1;
            }
        }
        let next = String::from_utf8(bytes).map_err(|_| invalid())?;
        if next == decoded {
            break;
        }
        decoded = next;
    }
    ensure!(
        !decoded.contains('\0')
            && !decoded
                .as_bytes()
                .windows(3)
                .any(|w| w[0] == b'%' && w[1].is_ascii_hexdigit() && w[2].is_ascii_hexdigit()),
        invalid()
    );
    let decoded = decoded.replace('\\', "/");
    let parts: Vec<_> = decoded.split('/').collect();
    ensure!(
        parts.first() == Some(&"partials")
            && !parts
                .iter()
                .any(|p| p.is_empty() || *p == "." || *p == ".."),
        invalid()
    );
    Ok(format!(".paseo/workflows/{}", parts.join("/")))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn partials_reject_nested_encoding_traversal() {
        assert_eq!(
            partial_path("w", "partials/a.txt").unwrap(),
            ".paseo/workflows/partials/a.txt"
        );
        for path in [
            "partials/%252e%252e/secret",
            "/etc/passwd",
            "partials\\..\\secret",
            "partials/%FF",
            "partials/%",
            "partials//x",
        ] {
            assert!(partial_path("w", path).is_err());
        }
    }
    #[tokio::test]
    async fn project_reads_only_referenced_safe_partials() {
        let root = tempfile::tempdir().unwrap();
        tokio::fs::create_dir_all(root.path().join(".paseo/workflows/partials"))
            .await
            .unwrap();
        tokio::fs::write(root.path().join(".paseo/hub.yml"), "name: demo\n")
            .await
            .unwrap();
        tokio::fs::write(
            root.path().join(".paseo/workflows/a.yml"),
            "steps:\n  - prompt:\n      - include: partials/a.txt\n",
        )
        .await
        .unwrap();
        tokio::fs::write(
            root.path().join(".paseo/workflows/partials/a.txt"),
            "prompt",
        )
        .await
        .unwrap();
        tokio::fs::write(
            root.path().join(".paseo/workflows/partials/unreferenced"),
            "private",
        )
        .await
        .unwrap();
        let (bundle, count) = project(root.path(), "demo").await.unwrap();
        assert_eq!(count, 1);
        assert_eq!(bundle["files"].as_array().unwrap().len(), 3);
        assert!(!bundle.to_string().contains("private"));
    }
}
