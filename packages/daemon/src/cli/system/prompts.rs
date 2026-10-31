mod password;
use super::error;
use anyhow::{Result, ensure};
pub(super) use password::read as password;
use std::io::IsTerminal;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;
static INPUT: tokio::sync::OnceCell<
    tokio::sync::Mutex<tokio::io::BufReader<crate::desktop_host::input::Input>>,
> = tokio::sync::OnceCell::const_new();

pub(super) fn interactive() -> bool {
    std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
}
pub(super) async fn text(
    message: &str,
    initial: Option<&str>,
    cancel: &CancellationToken,
) -> Result<String> {
    loop {
        let prompt = match initial.filter(|s| !s.is_empty()) {
            Some(value) => format!("{message} [{value}]: "),
            None => format!("{message}: "),
        };
        let mut stderr = tokio::io::stderr();
        stderr.write_all(prompt.as_bytes()).await?;
        stderr.flush().await?;
        let shared = INPUT
            .get_or_try_init(|| async {
                Ok::<_, std::io::Error>(tokio::sync::Mutex::new(tokio::io::BufReader::new(
                    crate::desktop_host::input::Input::open()?,
                )))
            })
            .await?;
        let mut input = String::new();
        let mut reader = shared.lock().await;
        let count = tokio::select! {_=cancel.cancelled()=>return Err(error("PROMPT_CANCELLED","Operation cancelled.")),count=reader.read_line(&mut input)=>count?};
        ensure!(count > 0, error("PROMPT_CANCELLED", "Operation cancelled."));
        let input = input.trim();
        let input = if input.is_empty() {
            initial.unwrap_or("")
        } else {
            input
        };
        if !input.is_empty() {
            return Ok(input.to_owned());
        }
        eprintln!("A value is required.");
    }
}
pub(super) async fn confirm(
    message: &str,
    default: bool,
    cancel: &CancellationToken,
) -> Result<bool> {
    loop {
        match text(
            &format!("{message} {}", if default { "[Y/n]" } else { "[y/N]" }),
            Some(if default { "yes" } else { "no" }),
            cancel,
        )
        .await?
        .to_lowercase()
        .as_str()
        {
            "yes" | "y" => return Ok(true),
            "no" | "n" => return Ok(false),
            _ => eprintln!("Enter yes or no."),
        }
    }
}
pub(super) async fn select(
    message: &str,
    options: &[(String, String)],
    suggested: Option<&str>,
    cancel: &CancellationToken,
) -> Result<String> {
    ensure!(!options.is_empty(), "No choices available");
    eprintln!("{message}");
    for (i, (id, label)) in options.iter().enumerate() {
        eprintln!(
            "  {}. {label}{}",
            i + 1,
            if suggested == Some(id.as_str()) {
                " (suggested)"
            } else {
                ""
            }
        );
    }
    let default = options
        .iter()
        .position(|(id, _)| suggested == Some(id.as_str()))
        .unwrap_or(0)
        + 1;
    loop {
        let answer = text("Select a number", Some(&default.to_string()), cancel).await?;
        if let Ok(index) = answer.parse::<usize>() {
            if let Some((id, _)) = index.checked_sub(1).and_then(|index| options.get(index)) {
                return Ok(id.clone());
            }
        }
        if options.iter().any(|(id, _)| id == &answer) {
            return Ok(answer);
        }
        eprintln!("Select one of the listed choices.");
    }
}
