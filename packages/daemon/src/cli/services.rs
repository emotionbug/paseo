mod projects;
mod schedules;
mod terminals;
use super::{Context, output::Output};
use anyhow::Result;
use clap::{ArgMatches, Command};
pub fn commands() -> Vec<Command> {
    projects::commands()
        .into_iter()
        .chain(terminals::commands())
        .chain(schedules::commands())
        .collect()
}
pub async fn execute(
    path: &[&str],
    matches: &ArgMatches,
    context: &Context,
) -> Result<Option<Output>> {
    if let Some(value) = projects::execute(path, matches, context).await? {
        return Ok(Some(value));
    }
    if let Some(value) = terminals::execute(path, matches, context).await? {
        return Ok(Some(value));
    }
    schedules::execute(path, matches, context).await
}
