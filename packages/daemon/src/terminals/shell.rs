use anyhow::Result;
use portable_pty::CommandBuilder;
use std::{collections::BTreeMap, path::Path};

const ZSH_ENV: &str = r#"typeset -g PASEO_SHELL_INTEGRATION_DIR="${${(%):-%N}:A:h}"
if [[ -n "${PASEO_ZSH_ZDOTDIR-}" ]]; then
  export ZDOTDIR="${PASEO_ZSH_ZDOTDIR}"
else
  unset ZDOTDIR
fi
if [[ -n "${ZDOTDIR-}" ]]; then
  if [[ -f "${ZDOTDIR}/.zshenv" ]]; then source "${ZDOTDIR}/.zshenv"; fi
elif [[ -f "${HOME}/.zshenv" ]]; then
  source "${HOME}/.zshenv"
fi
source "${PASEO_SHELL_INTEGRATION_DIR}/paseo-integration.zsh"
"#;
const ZSH_INTEGRATION: &str = r#"if [[ -n "${_PASEO_ZSH_INTEGRATION_LOADED-}" ]]; then return; fi
typeset -g _PASEO_ZSH_INTEGRATION_LOADED=1
autoload -Uz add-zsh-hook
typeset -g _PASEO_ZSH_COMMAND_ACTIVE=0
function _paseo_osc633() { printf '\e]633;%s\a' "$1"; }
function _paseo_precmd() {
  local command_status=$?
  if [[ "$_PASEO_ZSH_COMMAND_ACTIVE" == "1" ]]; then
    _paseo_osc633 "D;${command_status}"
    _PASEO_ZSH_COMMAND_ACTIVE=0
  fi
  printf '\e]2;%s\a' "${PWD/#$HOME/~}"
  _paseo_osc633 "A"
}
function _paseo_preexec() {
  _PASEO_ZSH_COMMAND_ACTIVE=1
  _paseo_osc633 "B"
  _paseo_osc633 "C"
  printf '\e]2;%s\a' "$1"
}
add-zsh-hook precmd _paseo_precmd
add-zsh-hook preexec _paseo_preexec
"#;

pub(super) fn prepare(
    shell: &str,
    args: &[String],
    env: &mut BTreeMap<String, String>,
) -> Result<Option<tempfile::TempDir>> {
    let name = Path::new(shell)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(shell);
    if name == "zsh" {
        let assets = tempfile::Builder::new().prefix("paseo-zsh-").tempdir()?;
        std::fs::write(assets.path().join(".zshenv"), ZSH_ENV)?;
        std::fs::write(assets.path().join("paseo-integration.zsh"), ZSH_INTEGRATION)?;
        env.insert(
            "PASEO_ZSH_ZDOTDIR".into(),
            env.get("ZDOTDIR").cloned().unwrap_or_default(),
        );
        env.insert(
            "ZDOTDIR".into(),
            assets.path().to_string_lossy().into_owned(),
        );
        return Ok(Some(assets));
    }
    #[cfg(windows)]
    {
        let resolved = which::which_in(shell, env.get("PATH"), std::env::current_dir()?)
            .unwrap_or_else(|_| shell.into());
        let extension = resolved.extension().and_then(|s| s.to_str()).unwrap_or("");
        if extension.eq_ignore_ascii_case("cmd") || extension.eq_ignore_ascii_case("bat") {
            let assets = tempfile::Builder::new().prefix("paseo-cmd-").tempdir()?;
            let mut script = format!("@echo off\r\n{}", escape_meta(&resolved.to_string_lossy()));
            for arg in args {
                script.push(' ');
                script.push_str(&escape_argument(arg));
            }
            script.push_str("\r\n");
            std::fs::write(assets.path().join("launch.cmd"), script)?;
            return Ok(Some(assets));
        }
    }
    let _ = args;
    Ok(None)
}
pub(super) fn command(
    shell: &str,
    args: &[String],
    env: &BTreeMap<String, String>,
    assets: Option<&Path>,
) -> Result<CommandBuilder> {
    #[cfg(windows)]
    if let Some(assets) = assets {
        if assets.join("launch.cmd").is_file() {
            let mut command = CommandBuilder::new(
                env.get("ComSpec")
                    .or_else(|| env.get("COMSPEC"))
                    .map(String::as_str)
                    .unwrap_or("cmd.exe"),
            );
            command.args(["/d", "/s", "/c"]);
            command.arg(assets.join("launch.cmd"));
            return Ok(command);
        }
    }
    let _ = (env, assets);
    let mut command = CommandBuilder::new(shell);
    command.args(args);
    Ok(command)
}
#[cfg(windows)]
fn escape_meta(value: &str) -> String {
    let mut output = String::new();
    for ch in value.chars() {
        if "()%!^\"`<>&|;, *?".contains(ch) {
            output.push('^');
        }
        output.push(ch);
    }
    output
}
#[cfg(windows)]
fn escape_argument(value: &str) -> String {
    let normalized = value.replace("\r\n", " ").replace(['\r', '\n'], " ");
    let mut escaped = String::from("\"");
    let mut slashes = 0;
    for ch in normalized.chars() {
        if ch == '\\' {
            slashes += 1;
            continue;
        }
        if ch == '\"' {
            escaped.extend(std::iter::repeat_n('\\', slashes * 2 + 1));
        } else {
            escaped.extend(std::iter::repeat_n('\\', slashes));
        }
        slashes = 0;
        escaped.push(ch);
    }
    escaped.extend(std::iter::repeat_n('\\', slashes * 2));
    escaped.push('"');
    escape_meta(&escape_meta(&escaped))
}
