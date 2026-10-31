use super::super::output::{self, Format};
use super::*;
use tokio_util::sync::CancellationToken;

pub(super) async fn history(
    client: &DaemonClient,
    context: &Context,
    id: &str,
    timeout: Duration,
) -> Result<Vec<Value>> {
    let payload=client.request_cancellable(json!({"type":"fetch_agent_timeline_request","agentId":id,"direction":"tail","limit":0,"projection":"projected"}),"fetch_agent_timeline_response",timeout,context.cancel.clone()).await?;
    Ok(response_array(&payload, "entries")?
        .iter()
        .map(|e| e["item"].clone())
        .collect())
}
pub(super) fn transcript(items: &[Value], tail: Option<usize>) -> Result<String> {
    if tail == Some(0) {
        return Ok(String::new());
    }
    let options = tail
        .map(|max| json!({"maxItems":max}))
        .unwrap_or_else(|| json!({}));
    let entries = crate::activity::curate(items, &options)?;
    if entries.is_empty() {
        return Ok("No activity to display.".into());
    }
    Ok(entries
        .iter()
        .map(|e| text(e, "text"))
        .collect::<Vec<_>>()
        .join("\n"))
}
fn matches(item: &Value, filter: Option<&str>) -> bool {
    let Some(filter) = filter else { return true };
    let filter = filter.to_ascii_lowercase();
    let kind = text(item, "type").to_ascii_lowercase();
    match filter.as_str() {
        "tools" => kind == "tool_call",
        "text" => matches!(
            kind.as_str(),
            "user_message" | "assistant_message" | "reasoning"
        ),
        "errors" => kind == "error",
        "permissions" => kind.contains("permission"),
        _ => kind.contains(&filter),
    }
}
fn timestamp(raw: &str) -> Result<i64> {
    chrono::DateTime::parse_from_rfc3339(raw)
        .map(|date| date.timestamp_millis())
        .or_else(|_| {
            chrono::NaiveDate::parse_from_str(raw, "%Y-%m-%d").map(|date| {
                date.and_hms_opt(0, 0, 0)
                    .unwrap()
                    .and_utc()
                    .timestamp_millis()
            })
        })
        .map_err(|_| {
            error(
                "INVALID_TIMESTAMP",
                format!(
                    "Invalid --since value: {raw}. Use a timestamp such as 2026-01-01T00:00:00Z."
                ),
            )
        })
}
async fn entries(
    client: &DaemonClient,
    context: &Context,
    id: &str,
    timeout: Duration,
    since: Option<i64>,
    filter: Option<&str>,
) -> Result<Vec<Value>> {
    let payload=client.request_cancellable(json!({"type":"fetch_agent_timeline_request","agentId":id,"direction":"tail","limit":0,"projection":"projected"}),"fetch_agent_timeline_response",timeout,context.cancel.clone()).await?;
    Ok(response_array(&payload, "entries")?
        .iter()
        .filter(|entry| {
            since.is_none_or(|since| {
                timestamp(text(entry, "timestamp")).is_ok_and(|date| date >= since)
            }) && matches(&entry["item"], filter)
        })
        .map(|entry| entry["item"].clone())
        .collect())
}
pub(super) async fn logs(
    m: &ArgMatches,
    context: &Context,
    client: &DaemonClient,
    attach: bool,
) -> Result<Output> {
    let id = fetch(client, context, required(m, "id")?).await?;
    let id = text(&id, "id");
    if attach {
        attach_stream(client, context, id, context.cancel.clone()).await?;
        return Ok(Output::new(Value::Null).silent());
    }
    let tail = string(m, "tail")
        .map(|v| {
            v.parse::<usize>().map_err(|_| {
                error(
                    "INVALID_TAIL",
                    format!("Invalid --tail value: {v}; expected a non-negative integer"),
                )
            })
        })
        .transpose()?;
    let since = string(m, "since").map(timestamp).transpose()?;
    let filter = string(m, "filter");
    if flag(m, "follow") {
        follow(
            client,
            context,
            id,
            filter,
            since,
            tail.or(Some(10)),
            context.cancel.clone(),
        )
        .await?;
        return Ok(Output::new(Value::Null).silent());
    }
    let items = entries(client, context, id, Duration::from_secs(60), since, filter).await?;
    let human = transcript(&items, tail)?;
    let items = if let Some(tail) = tail {
        items
            .into_iter()
            .rev()
            .take(tail)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect()
    } else {
        items
    };
    Ok(Output::new(json!(items)).human(human))
}
struct Chain {
    kind: String,
    turn: Value,
    message: Value,
    buffer: String,
    emitted: bool,
}
#[derive(Default)]
struct Writer {
    chain: Option<Chain>,
}
impl Writer {
    fn end(&mut self) -> Result<()> {
        if let Some(chain) = self.chain.take() {
            if chain.emitted {
                output::stdout_chunk("\n")?;
            }
        }
        Ok(())
    }
    fn push(&mut self, item: &Value, turn: &Value, filter: Option<&str>) -> Result<()> {
        if !matches(item, filter) {
            return self.end();
        }
        let kind = text(item, "type");
        if !matches!(kind, "assistant_message" | "reasoning") {
            self.end()?;
            let rendered = transcript(std::slice::from_ref(item), None)?;
            if rendered != "No activity to display." {
                output::stdout(&rendered)?;
            }
            return Ok(());
        }
        let same = self.chain.as_ref().is_some_and(|chain| {
            chain.kind == kind
                && chain.turn == *turn
                && (kind != "assistant_message"
                    || item["messageId"].is_null()
                    || chain.message == item["messageId"])
        });
        if !same {
            self.end()?;
            self.chain = Some(Chain {
                kind: kind.into(),
                turn: turn.clone(),
                message: item["messageId"].clone(),
                buffer: String::new(),
                emitted: false,
            });
        }
        let chain = self.chain.as_mut().unwrap();
        chain.buffer.push_str(text(item, "text"));
        if !chain.emitted {
            let trimmed = chain.buffer.trim_start();
            if trimmed.len() != chain.buffer.len() {
                chain.buffer = trimmed.into();
            }
        }
        let content = chain.buffer.trim_end().len();
        if content > 0 {
            if !chain.emitted && kind == "reasoning" {
                output::stdout_chunk("[Thought] ")?;
            }
            output::stdout_chunk(&chain.buffer[..content])?;
            chain.emitted = true;
            chain.buffer.drain(..content);
        }
        Ok(())
    }
}
pub(super) async fn follow(
    client: &DaemonClient,
    context: &Context,
    id: &str,
    filter: Option<&str>,
    since: Option<i64>,
    tail: Option<usize>,
    stop: CancellationToken,
) -> Result<()> {
    let items = entries(client, context, id, Duration::from_secs(2), since, filter).await;
    match items {
        Ok(items) => {
            if context.output.format == Format::Json {
                let tail = tail.unwrap_or(items.len());
                for item in items.iter().skip(items.len().saturating_sub(tail)) {
                    output::stdout(&serde_json::to_string(item)?)?;
                }
            } else {
                let rendered = transcript(&items, tail)?;
                if !rendered.is_empty() && rendered != "No activity to display." {
                    output::stdout(&rendered)?;
                }
            }
        }
        Err(error) => eprintln!("Warning: failed to fetch existing timeline: {error}"),
    }
    let mut receiver = client.subscribe();
    let subscription = rpc(
        client,
        context,
        json!({"type":"agent.timeline.set_subscription.request","agentIds":[id]}),
        "agent.timeline.set_subscription.response",
    )
    .await?;
    let subscription_id = subscription["subscriptionId"]
        .as_str()
        .ok_or_else(|| anyhow!("Host did not assign timeline subscription ownership"))?;
    let mut writer = Writer::default();
    let result:Result<()>=async{
        if context.output.format!=Format::Json{
            let count=tail.unwrap_or(10);let label=if count==0{"no history".into()}else{format!("last {count} entr{}",if count==1{"y"}else{"ies"})};
            output::stdout(&format!("\n--- Following logs ({label}; Ctrl+C to stop) ---\n"))?;
        }
        loop{let event=tokio::select!{biased;_=stop.cancelled()=>break,_=context.cancel.cancelled()=>break,event=receiver.recv()=>event?};
        if event["payload"]["subscriptionId"]!=subscription_id{continue;}
        if event["type"]=="subscription.error"{return Err(anyhow!("Timeline observation stopped: {}",event["payload"]["error"].as_str().unwrap_or("Subscription closed")));}
        if event["type"]=="agent.timeline.replacement"{writer.end()?;if context.output.format==Format::Json{output::stdout(&serde_json::to_string(&event)?)?;}else{output::stdout("\n[Timeline replaced; earlier output is no longer current]")?;}continue;}
        if event["type"]!="agent_stream"||event["payload"]["agentId"]!=id{continue;}let update=&event["payload"]["event"];
        if update["type"]=="timeline"{if since.is_some_and(|since|!timestamp(text(&event["payload"],"timestamp")).is_ok_and(|date|date>=since)){continue;}
            if context.output.format==Format::Json{if matches(&update["item"],filter){output::stdout(&serde_json::to_string(&update["item"])?)?;}}else{writer.push(&update["item"],&update["turnId"],filter)?;}
        }else if matches!(text(update,"type"),"turn_completed"|"turn_failed"|"turn_canceled"){writer.end()?;}
    }Ok(())}.await;
    let flush = writer.end();
    let released = client.release_subscription(subscription_id).await;
    if context.cancel.is_cancelled() {
        return Ok(());
    }
    result?;
    flush?;
    released?;
    Ok(())
}
fn attached_item(item: &Value) -> Result<()> {
    match text(item, "type") {
        "assistant_message" => output::stdout_chunk(text(item, "text"))?,
        "reasoning" => output::stdout(&format!("\n[Reasoning] {}", text(item, "text")))?,
        "tool_call" => output::stdout(&format!(
            "\n[Tool: {}] {}",
            text(item, "name"),
            item["status"].as_str().unwrap_or("started")
        ))?,
        "todo" => output::stdout(&format!(
            "\n[Todo] {}/{} completed",
            array(&item["items"])
                .iter()
                .filter(|i| i["completed"] == true)
                .count(),
            array(&item["items"]).len()
        ))?,
        "error" => eprintln!("\n[Error] {}", text(item, "message")),
        "user_message" => output::stdout(&format!("\n[User] {}", text(item, "text")))?,
        _ => {}
    }
    Ok(())
}
pub(super) async fn attach_stream(
    client: &DaemonClient,
    context: &Context,
    id: &str,
    stop: CancellationToken,
) -> Result<()> {
    let structured = context.output.format == Format::Json;
    if !structured {
        output::stdout(&format!(
            "Fetching history for agent {}...\n(Press Ctrl+C to detach)\n",
            short(id)
        ))?;
    }
    match history(client, context, id, Duration::from_secs(2)).await {
        Ok(items) => {
            for item in items {
                if structured {
                    output::stdout(&serde_json::to_string(&item)?)?;
                } else {
                    attached_item(&item)?;
                }
            }
        }
        Err(error) => eprintln!("Warning: failed to fetch existing timeline: {error}"),
    }
    let mut receiver = client.subscribe();
    let owned = rpc(
        client,
        context,
        json!({"type":"agent.timeline.set_subscription.request","agentIds":[id]}),
        "agent.timeline.set_subscription.response",
    )
    .await?;
    let ownership = owned["subscriptionId"]
        .as_str()
        .ok_or_else(|| anyhow!("Host did not assign timeline ownership"))?;
    let result:Result<()>=async{
        if !structured{output::stdout(&format!("Attached to agent {}.",short(id)))?;}
        loop{
            let message=tokio::select!{_=stop.cancelled()=>break,_=context.cancel.cancelled()=>break,message=receiver.recv()=>message?};
            if message["payload"]["subscriptionId"]!=ownership{continue;}
            if structured{output::stdout(&serde_json::to_string(&message)?)?;continue;}
            if message["type"]=="agent.timeline.replacement"{output::stdout("\n[Timeline replaced; earlier output is no longer current]")?;continue;}
            if message["type"]=="subscription.error"{return Err(anyhow!("Timeline observation stopped: {}",text(&message["payload"],"error")));}
            if message["type"]!="agent_stream"{continue;}
            let event=&message["payload"]["event"];
            match text(event,"type"){
                "timeline"=>attached_item(&event["item"])?,
                "permission_requested"=>{output::stdout(&format!("\n[Permission Required] {}",text(&event["request"],"name")))?;if let Some(description)=event["request"]["description"].as_str(){output::stdout(&format!("  {description}"))?;}},
                "permission_resolved"=>output::stdout(&format!("\n[Permission {}]",text(&event["resolution"],"behavior")))?,
                "turn_failed"=>eprintln!("\n[Turn Failed] {}",text(event,"error")),
                "attention_required"=>output::stdout(&format!("\n[Attention Required: {}]",text(event,"reason")))?,
                _=>{}
            }
        }Ok(())
    }.await;
    let released = client.release_subscription(ownership).await;
    if !structured {
        output::stdout("\n\nDetaching from agent...")?;
    }
    if context.cancel.is_cancelled() {
        return Ok(());
    }
    result?;
    released?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn filters_include_reasoning_and_unknown_event_substrings() {
        assert!(matches(&json!({"type":"reasoning"}), Some("text")));
        assert!(!matches(&json!({"type":"tool_call"}), Some("permissions")));
        assert!(matches(
            &json!({"type":"permission_requested"}),
            Some("permissions")
        ));
    }
    #[test]
    fn tail_zero_has_no_activity_sentinel() {
        assert_eq!(transcript(&[], Some(0)).unwrap(), "");
    }
    #[test]
    fn since_accepts_date_or_rfc3339() {
        assert_eq!(
            timestamp("2026-01-01").unwrap(),
            timestamp("2026-01-01T00:00:00Z").unwrap()
        );
        assert!(timestamp("yesterdayish").is_err());
    }
}
