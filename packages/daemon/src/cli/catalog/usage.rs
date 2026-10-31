use super::*;
async fn reports(
    client: &DaemonClient,
    context: &Context,
    agent: Option<&str>,
    ids: Option<&str>,
    refresh: bool,
) -> Result<Vec<Value>> {
    let id = uuid::Uuid::new_v4().to_string();
    let mut events = client.subscribe();
    let mut request = json!({"type":"usage.list_reports.request","requestId":id});
    if let Some(agent) = agent {
        request["agentId"] = json!(agent);
    }
    if let Some(id) = ids {
        request["reportIds"] = json!([id]);
    }
    if refresh {
        request["forceRefresh"] = json!(true);
    }
    let response = client.request_cancellable(
        request,
        "usage.list_reports.response",
        Duration::from_secs(60),
        context.cancel.clone(),
    );
    tokio::pin!(response);
    let mut reports = Vec::new();
    loop {
        tokio::select! {biased;event=events.recv()=>{let event=event?;if event["payload"]["requestId"]!=id{continue;}if event["type"]=="usage.list_reports.update"{reports.push(event["payload"]["report"].clone());}else if event["type"]=="usage.list_reports.response"||event["type"]=="rpc_error"{response.await?;break;}},result=&mut response=>{result?;loop{match events.try_recv(){Ok(event)=>{if event["payload"]["requestId"]==id&&event["type"]=="usage.list_reports.update"{reports.push(event["payload"]["report"].clone());}},Err(tokio::sync::broadcast::error::TryRecvError::Empty)=>break,Err(e)=>return Err(e.into())}}break;}}
    }
    Ok(reports)
}
pub(super) async fn execute(
    command: &str,
    m: &ArgMatches,
    context: &Context,
    client: &DaemonClient,
) -> Result<Output> {
    if client.server_info()["features"]["usageSources"] != true {
        return Err(error(
            "UNSUPPORTED_HOST",
            "Update the host to use usage reports.",
        ));
    }
    if command == "inspect" {
        let id = required(m, "id")?;
        let entries = reports(client, context, None, None, false).await?;
        let entry = entries
            .into_iter()
            .find(|e| e["id"] == id)
            .ok_or_else(|| error("REPORT_NOT_FOUND", format!("Unknown usage report: {id}")))?;
        let entry = if flag(m, "refresh") {
            reports(client, context, None, Some(id), true)
                .await?
                .into_iter()
                .find(|e| e["id"] == id)
                .ok_or_else(|| error("REPORT_NOT_FOUND", format!("Unknown usage report: {id}")))?
        } else {
            entry
        };
        let human = inspect(&entry);
        return Ok(Output::new(entry).human(human));
    }
    let entries = reports(
        client,
        context,
        string(m, "agent"),
        None,
        flag(m, "refresh"),
    )
    .await?;
    let mut rows = Vec::new();
    for entry in &entries {
        for (index, usage) in summary(entry).into_iter().enumerate() {
            let first = index == 0;
            rows.push(json!({"id":if first{text(entry,"id")}else{""},"source":if first{text(entry,"sourceLabel")}else{""},"account":if first{value(&entry["account"]["label"])}else{String::new()},"status":if first{text(&entry["report"],"status")}else{""},"usage":usage,"fetched":if first{value(&entry["fetchedAt"])}else{String::new()}}));
        }
    }
    let human = if rows.is_empty() {
        "No usage reports returned".into()
    } else {
        let mut options = context.output.clone();
        options.format = Format::Table;
        options.quiet = false;
        Output::new(json!(rows))
            .columns(&[
                ("REPORT ID", "id"),
                ("SOURCE", "source"),
                ("ACCOUNT", "account"),
                ("STATUS", "status"),
                ("USAGE", "usage"),
                ("FETCHED (LATEST ATTEMPT)", "fetched"),
            ])
            .render(&options)?
    };
    Ok(Output::new(json!(entries)).human(human))
}
fn value(value: &Value) -> String {
    if value.is_null() {
        "-".into()
    } else {
        value
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| value.to_string())
    }
}
fn percent(v: &Value) -> String {
    if v.is_null() {
        "-".into()
    } else {
        format!("{}%", value(v))
    }
}
fn failure(report: &Value) -> String {
    if report["status"] == "error" {
        return text(report, "error").into();
    }
    if report["status"] != "unavailable" {
        return "-".into();
    }
    let problem = &report["problem"];
    if problem["kind"] == "no_quota" {
        return text(problem, "detail").into();
    }
    let remedy = problem["refreshedBy"]
        .as_str()
        .map(|v| format!("Run {v} to refresh login."))
        .unwrap_or_else(|| "Sign in again.".into());
    if problem["kind"] == "expired" {
        format!("Login expired at {}. {remedy}", text(problem, "expiresAt"))
    } else {
        format!(
            "Login rejected (HTTP {}). {remedy}",
            value(&problem["status"])
        )
    }
}
fn window(window: &Value) -> String {
    format!(
        "{}: {} used, {} remaining",
        text(window, "label"),
        percent(&window["usedPct"]),
        percent(&window["remainingPct"])
    )
}
fn balance(balance: &Value) -> String {
    format!(
        "{}: {} used, {} remaining, {} limit ({})",
        text(balance, "label"),
        value(&balance["used"]),
        value(&balance["remaining"]),
        value(&balance["limit"]),
        text(balance, "unit")
    )
}
fn available(report: &Value) -> Vec<String> {
    let mut lines: Vec<_> = array(&report["windows"])
        .iter()
        .map(window)
        .chain(array(&report["balances"]).iter().map(balance))
        .collect();
    if lines.is_empty() {
        lines.push("-".into());
    }
    lines
}
fn successful(entry: &Value) -> Option<&str> {
    if entry["report"]["status"] == "available" {
        entry["successfulAt"]
            .as_str()
            .or_else(|| entry["fetchedAt"].as_str())
    } else {
        entry["lastSuccessful"]["fetchedAt"].as_str()
    }
}
fn summary(entry: &Value) -> Vec<String> {
    let report = &entry["report"];
    if report["status"] == "available" {
        let mut lines = available(report);
        lines.push(format!(
            "Last successful: {}",
            successful(entry).unwrap_or("undefined")
        ));
        return lines;
    }
    let mut lines = vec![failure(report)];
    for login in array(&entry["loginErrors"]) {
        lines.push(format!(
            "{}: {}",
            text(login, "harness"),
            failure(&login["report"])
        ));
    }
    if entry["lastSuccessful"].is_object() {
        lines.push(format!(
            "Last successful: {}",
            text(&entry["lastSuccessful"], "fetchedAt")
        ));
        lines.extend(
            available(&entry["lastSuccessful"]["report"])
                .into_iter()
                .map(|s| format!("Retained: {s}")),
        );
    } else {
        lines.push("No successful update".into());
    }
    lines
}
fn inspect(entry: &Value) -> String {
    let mut lines = vec![
        format!("Report ID: {}", text(entry, "id")),
        format!("Source: {}", text(entry, "sourceLabel")),
        format!("Source ID: {}", text(entry, "sourceId")),
        format!("Account: {}", value(&entry["account"]["label"])),
        format!("Status: {}", text(&entry["report"], "status")),
        format!("Latest attempt: {}", value(&entry["fetchedAt"])),
    ];
    lines.push(
        successful(entry)
            .map(|v| format!("Last successful: {v}"))
            .unwrap_or_else(|| "No successful update".into()),
    );
    let available = entry["report"]["status"] == "available";
    if !available {
        lines.push(format!("Reason: {}", failure(&entry["report"])));
    }
    let report = if available {
        &entry["report"]
    } else {
        &entry["lastSuccessful"]["report"]
    };
    if report.is_object() {
        if !available {
            lines.push("Retained usage:".into());
        }
        lines.push(format!("Plan: {}", value(&report["planLabel"])));
        lines.push("Windows:".into());
        if array(&report["windows"]).is_empty() {
            lines.push("  -".into());
        }
        for w in array(&report["windows"]) {
            lines.push(format!("  {} [{}]", window(w), text(w, "id")));
            lines.push(format!("    Resets: {}", value(&w["resetsAt"])));
            if !w["runsOutAt"].is_null() {
                lines.push(format!("    Runs out: {}", value(&w["runsOutAt"])));
            }
            if !w["shortfallPct"].is_null() {
                lines.push(format!("    Shortfall: {}", percent(&w["shortfallPct"])));
            }
        }
        lines.push("Balances:".into());
        if array(&report["balances"]).is_empty() {
            lines.push("  -".into());
        }
        for b in array(&report["balances"]) {
            lines.push(format!("  {} [{}]", balance(b), text(b, "id")));
            lines.push(format!("    Resets: {}", value(&b["resetsAt"])));
        }
        lines.push("Details:".into());
        if array(&report["details"]).is_empty() {
            lines.push("  -".into());
        }
        for d in array(&report["details"]) {
            lines.push(format!("  {}: {}", text(d, "label"), value(&d["value"])));
        }
    }
    if !array(&entry["loginErrors"]).is_empty() {
        lines.push("Login failures:".into());
        for login in array(&entry["loginErrors"]) {
            lines.push(format!(
                "  {}: {}",
                text(login, "harness"),
                failure(&login["report"])
            ));
        }
    }
    lines.join("\n")
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn failed_refresh_keeps_success_timestamp_distinct() {
        let entry = json!({"id":"r","fetchedAt":"new","report":{"status":"error","error":"offline"},"lastSuccessful":{"fetchedAt":"old","report":{"status":"available","windows":[{"label":"Week","usedPct":10,"remainingPct":90}]}}});
        let lines = summary(&entry);
        assert!(lines.contains(&"Last successful: old".into()));
        assert!(
            lines
                .iter()
                .any(|s| s == "Retained: Week: 10% used, 90% remaining")
        );
        assert!(!lines.contains(&"Last successful: new".into()));
    }
}
