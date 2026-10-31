use super::{
    AutomationService,
    store::{Schedule, Target},
    string,
};
use anyhow::{Result, bail, ensure};
use serde_json::{Map, Value, json};

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub(super) struct TargetGone(pub String);
pub(super) struct ExecutionResult {
    pub agent_id: Option<String>,
    pub output: Option<String>,
}

fn actor() -> Value {
    json!({"kind":"system","source":"schedule"})
}
fn title(config: &Value, prompt: &str) -> String {
    if let Some(title) = config["title"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return title.to_owned();
    }
    let line = prompt
        .lines()
        .map(str::trim)
        .find(|s| !s.is_empty())
        .unwrap_or("");
    let normalized = line.split_whitespace().collect::<Vec<_>>().join(" ");
    String::from_utf16_lossy(&normalized.encode_utf16().take(60).collect::<Vec<_>>())
        .trim()
        .to_owned()
}
/// service.ts executeSchedule outcome checks. Existing-agent runs only inspect the wait
/// result; new-agent runs also observe the run itself (cancel, turn failure, timeline).
fn output(result: &Value, agent_id: &str, new_agent: bool) -> Result<Option<String>> {
    if new_agent && result["canceled"] == true {
        bail!("Scheduled agent {agent_id} was canceled");
    }
    if result
        .get("permission")
        .is_some_and(|v| !v.is_null() && v != &Value::Bool(false))
    {
        bail!("Scheduled agent {agent_id} is waiting for permission");
    }
    let last_message = result["lastMessage"].as_str();
    if result["status"] == "error" {
        let message = last_message.map(|message| {
            // runAgent rejects a new agent's failed turn with the formatted failure itself,
            // not the "[System Error]" row the manager appended for it.
            if new_agent {
                message
                    .strip_prefix("[System Error] ")
                    .unwrap_or(message)
                    .to_owned()
            } else {
                message.to_owned()
            }
        });
        bail!(
            "{}",
            message.unwrap_or_else(|| format!("Scheduled agent {agent_id} failed"))
        );
    }
    if !new_agent {
        return Ok(last_message
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned));
    }
    if let Some(s) = last_message.filter(|s| !s.trim().is_empty()) {
        return Ok(Some(s.to_owned()));
    }
    if let Some(s) = result["finalText"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return Ok(Some(s.to_owned()));
    }
    if let Some(timeline) = result["timeline"].as_array() {
        let curated = crate::activity::curate(timeline, &json!({}))?;
        let text = curated
            .iter()
            .filter_map(|v| v["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n");
        if !text.trim().is_empty() {
            return Ok(Some(text.trim().to_owned()));
        }
    }
    Ok(None)
}

impl AutomationService {
    pub(super) async fn execute(
        &self,
        schedule: &Schedule,
        run_id: &str,
    ) -> Result<ExecutionResult> {
        let app = self.app()?;
        match &schedule.target {
            Target::Agent { agent_id } => {
                let record = app
                    .agents
                    .agent_snapshot(agent_id)
                    .await?
                    .ok_or_else(|| TargetGone(format!("Agent {agent_id} no longer exists")))?;
                if !record["archivedAt"].is_null() {
                    return Err(TargetGone(format!("Agent {agent_id} is archived")).into());
                }
                let heading = if let Some(name) = &schedule.name {
                    format!(
                        "Schedule \"{name}\" fired (id={}, run={run_id}).",
                        schedule.id
                    )
                } else {
                    format!("Schedule fired (id={}, run={run_id}).", schedule.id)
                };
                let prompt = format!(
                    "<paseo-system>\n{heading}\n{}\n</paseo-system>",
                    schedule.prompt
                );
                let result = app.agents.schedule_run(agent_id, json!(prompt)).await?;
                Ok(ExecutionResult {
                    agent_id: Some(agent_id.clone()),
                    output: output(&result, agent_id, false)?,
                })
            }
            Target::NewAgent { config } => {
                let cwd = string(config, "cwd")?;
                match tokio::fs::metadata(cwd).await {
                    Ok(meta) if !meta.is_dir() => {
                        return Err(TargetGone(format!(
                            "Working directory {cwd} is not a directory"
                        ))
                        .into());
                    }
                    Ok(_) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        return Err(
                            TargetGone(format!("Working directory {cwd} no longer exists")).into(),
                        );
                    }
                    Err(e) => return Err(e.into()),
                }
                let source = if config["isolation"] == "worktree" {
                    json!({"kind":"worktree","cwd":cwd})
                } else {
                    json!({"kind":"directory","path":cwd})
                };
                let workspace = app
                    .projects
                    .create_workspace(
                        &json!({"source":source,"firstAgentContext":{"prompt":schedule.prompt}}),
                    )
                    .await?;
                let workspace_id = string(&workspace, "workspaceId")?;
                let workspace_cwd = string(&workspace, "cwd")?;
                let mut agent_id: Option<String> = None;
                let result=async {
                    self.record_workspace(&schedule.id,run_id,workspace_id,None).await?;
                    let mut run_config=Map::new();
                    for key in ["provider","modeId","model","thinkingOptionId","title","providerOptions","featureValues","systemPrompt","mcpServers"] {
                        if let Some(value)=config.get(key){run_config.insert(key.into(),value.clone());}
                    }
                    run_config.insert("cwd".into(),json!(workspace_cwd));
                    let provider=string(config,"provider")?;
                    let provider_model=if provider.contains('/') {provider.to_owned()}else if let Some(model)=config["model"].as_str().filter(|s|!s.is_empty()){format!("{provider}/{model}")}else{provider.to_owned()};
                    let mut input=json!({"kind":"mcp","provider":provider_model,"config":run_config,"cwd":workspace_cwd,"workspaceId":workspace_id,"title":title(config,&schedule.prompt),"labels":{"paseo.schedule-id":schedule.id,"paseo.schedule-run":run_id},"unattended":true,"promptFailure":"return-error","notifyOnFinish":false});
                    for (from,to) in [("modeId","mode"),("thinkingOptionId","thinking"),("featureValues","features")] {if let Some(value)=config.get(from){input[to]=value.clone();}}
                    let created=app.agents.create(&actor(),input).await?;
                    let id=string(&created["snapshot"],"id")?.to_owned();agent_id=Some(id.clone());
                    self.record_workspace(&schedule.id,run_id,workspace_id,Some(&id)).await?;
                    if let Some(error)=created.get("initialPromptError").filter(|v|!v.is_null()) {bail!("{}",error.as_str().map(str::to_owned).unwrap_or_else(||error.to_string()));}
                    ensure!(!self.stop.is_cancelled(),"Automation service is stopped");
                    let result=app.agents.schedule_run(&id,json!(schedule.prompt)).await?;
                    Ok(ExecutionResult {agent_id:Some(id.clone()),output:output(&result,&id,true)?})
                }.await;
                if agent_id.is_none() || config["archiveOnFinish"].as_bool().unwrap_or(true) {
                    self.archive_workspace_best_effort(workspace_id).await;
                }
                result
            }
        }
    }
    pub(super) async fn archive_workspace_best_effort(&self, workspace_id: &str) {
        let result = async {
            self.app()?
                .projects
                .archive_workspace(&actor(), workspace_id)
                .await
                .map(|_| ())
        }
        .await;
        if let Err(error) = result {
            tracing::warn!(%error,%workspace_id,"Failed to archive scheduled workspace after run");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn existing_agent_runs_ignore_cancel_and_trim_the_reply() {
        let result = json!({"status":"idle","canceled":true,"lastMessage":"  done  ","timeline":[{"type":"assistant_message","text":"older"}]});
        assert_eq!(output(&result, "a", false).unwrap().as_deref(), Some("done"));
        assert!(output(&result, "a", true).is_err());
        let blank = json!({"status":"idle","lastMessage":" ","timeline":[{"type":"assistant_message","text":"older"}]});
        assert_eq!(output(&blank, "a", false).unwrap(), None);
    }

    #[test]
    fn new_agent_failures_report_the_turn_failure_text() {
        let failed = json!({"status":"error","lastMessage":"[System Error] boom\n\ncode: E1"});
        assert_eq!(
            output(&failed, "a", true).unwrap_err().to_string(),
            "boom\n\ncode: E1"
        );
        assert_eq!(
            output(&failed, "a", false).unwrap_err().to_string(),
            "[System Error] boom\n\ncode: E1"
        );
    }
}
