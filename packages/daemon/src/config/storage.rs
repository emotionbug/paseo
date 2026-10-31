use anyhow::{Context, Result};
use serde_json::Value;
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};

pub fn read(home: &Path, initialize: bool, defaults_if_missing: bool) -> Result<Value> {
    let path = home.join("config.json");
    let mut file = match fs::File::open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let defaults = if initialize || defaults_if_missing {
                super::schema::defaults()
            } else {
                serde_json::json!({})
            };
            if initialize {
                save(home, &defaults)?;
            }
            return Ok(defaults);
        }
        Err(error) => return Err(error).context("[Config] Failed to read configuration"),
    };
    if initialize {
        private_file(&path);
    }
    let mut raw = String::new();
    file.read_to_string(&mut raw)
        .context("[Config] Failed to read configuration")?;
    parse_document(&path, &raw)
}

/// The daemon's persisted file is JSON. Explicit YAML documents (for import/edit
/// callers) use the same schema; a malformed JSON file is never retried as YAML.
pub fn parse_document(path: &Path, raw: &str) -> Result<Value> {
    let raw = raw.strip_prefix('\u{feff}').unwrap_or(raw);
    let value = match path.extension().and_then(|s| s.to_str()) {
        Some("yaml" | "yml") => {
            serde_yaml::from_str(raw).context("[Config] Invalid YAML configuration")?
        }
        _ => serde_json::from_str(raw).context("[Config] Invalid JSON configuration")?,
    };
    super::schema::normalize_persisted(value)
}

pub fn save(home: &Path, value: &Value) -> Result<()> {
    let value = super::schema::normalize_persisted(value.clone())?;
    let mut raw = serde_json::to_vec_pretty(&value)?;
    raw.push(b'\n');
    atomic_private_write(&home.join("config.json"), &raw)
}

pub fn atomic_private_write(path: &Path, raw: &[u8]) -> Result<()> {
    let parent = path.parent().context("Configuration path has no parent")?;
    fs::create_dir_all(parent).context("[Config] Failed to create configuration directory")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(parent, fs::Permissions::from_mode(0o700));
    }
    // Preserve an existing symlink rather than replacing the user's link.
    let destination: PathBuf = match fs::canonicalize(path) {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => path.to_path_buf(),
        Err(error) => return Err(error).context("[Config] Failed to resolve configuration path"),
    };
    let directory = destination
        .parent()
        .context("Configuration target has no parent")?;
    let mut temporary = tempfile::Builder::new()
        .prefix(".paseo-config-")
        .tempfile_in(directory)
        .context("[Config] Failed to create private configuration file")?;
    private_file(temporary.path());
    temporary
        .write_all(raw)
        .context("[Config] Failed to write configuration")?;
    temporary
        .as_file()
        .sync_all()
        .context("[Config] Failed to sync configuration")?;
    temporary
        .persist(&destination)
        .map_err(|error| error.error)
        .context("[Config] Failed to replace configuration")?;
    private_file(&destination);
    Ok(())
}

fn private_file(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    let _ = path;
}
