use super::*;
use base64::Engine;
use regex::Regex;

pub(super) fn matches(pattern: &str, text: &str) -> bool {
    Regex::new(&format!("(?i){pattern}"))
        .expect("mock regex")
        .is_match(text)
}
fn captures(pattern: &str, text: &str) -> Option<Vec<String>> {
    Regex::new(&format!("(?i){pattern}"))
        .expect("mock regex")
        .captures(text)
        .map(|c| {
            c.iter()
                .map(|v| v.map(|s| s.as_str().to_owned()).unwrap_or_default())
                .collect()
        })
}
fn number(pattern: &str, text: &str, cap: u64) -> Option<u64> {
    captures(pattern, text)
        .and_then(|c| c[1].parse::<u64>().ok())
        .filter(|n| *n > 0 && *n <= 9_007_199_254_740_991)
        .map(|n| n.min(cap))
}
pub(super) fn usage(cycle: u64, tokens: u64) -> Value {
    json!({"inputTokens":cycle*32,"outputTokens":tokens,"contextWindowUsedTokens":tokens*2,"contextWindowMaxTokens":128000})
}
fn payload_usage(n: u64) -> Value {
    json!({"inputTokens":1,"outputTokens":n,"contextWindowUsedTokens":n,"contextWindowMaxTokens":128000})
}
fn tool(call_id: String, name: &str, status: &str, detail: Value) -> Value {
    json!({"type":"tool_call","callId":call_id,"name":name,"status":status,"error":null,"detail":detail})
}
fn tokenize(text: &str) -> Vec<String> {
    static TOKENS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(\s*)(\S+)|(\s+)").unwrap());
    let mut out = vec![];
    for c in TOKENS.captures_iter(text) {
        if let Some(ws) = c.get(3) {
            out.push(ws.as_str().into());
            continue;
        }
        let ws = c.get(1).map(|m| m.as_str()).unwrap_or("");
        let word = c[2].encode_utf16().collect::<Vec<_>>();
        if word.len() <= 5 {
            out.push(format!("{ws}{}", &c[2]));
        } else {
            for (i, chunk) in word.chunks(4).enumerate() {
                out.push(format!(
                    "{}{}",
                    if i == 0 { ws } else { "" },
                    String::from_utf16_lossy(chunk)
                ));
            }
        }
    }
    out
}
fn burst_size(sequence: u64) -> u64 {
    // JS first multiply is a Number, followed by ToInt32 at XOR.
    let product = (sequence + 1) as f64 * 2654435761_f64;
    let mut hash = product as u64 as u32;
    hash ^= hash >> 15;
    hash = hash.wrapping_mul(2246822519);
    hash ^= hash >> 13;
    1 + u64::from(hash % 40)
}
fn structured(text: &str) -> Option<String> {
    let fields = text.contains("\"title\"") && text.contains("\"branch\"");
    if !(text.contains("Generate a title and a git branch name for a coding agent")
        && (text.contains("Return JSON only with fields 'title' and 'branch'.")
            || text.contains("\"title\"")
            || text.contains("\"branch\"")))
        && !(text.contains("You must respond with JSON only that matches this JSON Schema")
            && fields)
    {
        return None;
    }
    let seed = ["user-prompt", "attachments"]
        .into_iter()
        .find_map(|tag| {
            text.split_once(&format!("<{tag}>\n"))
                .and_then(|(_, s)| s.split_once(&format!("\n</{tag}>")).map(|(v, _)| v.trim()))
        })
        .unwrap_or("");
    let first = seed
        .lines()
        .find(|s| !s.trim().is_empty())
        .unwrap_or("Mock task")
        .trim()
        .trim_matches(['\"', '\'', '`']);
    let title = first
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(80)
        .collect::<String>()
        .trim()
        .to_owned();
    let branch = Regex::new("[^a-z0-9]+")
        .unwrap()
        .replace_all(&title.to_lowercase(), "-")
        .trim_matches('-')
        .chars()
        .take(100)
        .collect::<String>();
    Some(format!(
        "{{\"title\":{},\"branch\":{}}}",
        json!(if title.is_empty() {
            "Mock task"
        } else {
            &title
        }),
        json!(if branch.is_empty() {
            "mock-task"
        } else {
            &branch
        })
    ))
}
fn questions(text: &str) -> Value {
    if matches(r"free[-\s]?write|freeform|text[-\s]?only", text) {
        return json!([
            {"question":"What is the GitHub private repo URL to push to?","header":"repoUrl","options":[],"multiSelect":false,"placeholder":"git@github.com:user/repo.git"},
            {"question":"What should the first commit message be?","header":"commitMessage","options":[],"multiSelect":false,"placeholder":"Initial commit"}
        ]);
    }
    let preview = format!(
        "Ship it dark behind the new-surface flag, then turn it on for internal users before everyone else.\n\nif (flags.enabled(\"new-surface\")) {{\n  render(<NewSurface />);\n}}\n{}\n// LAST PREVIEW LINE",
        (1..=40)
            .map(|i| format!("// rollout step {i}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
    json!([
        {"question":"Which surface should this apply to?","header":"surface","options":[{"label":"App"},{"label":"Desktop"}],"multiSelect":false},
        {"question":"Which rollout should we use?","header":"rollout","options":[{"label":"Immediately"},{"label":"Behind feature flag","preview":preview}],"multiSelect":false},
        {"question":"What success criteria should we use?","header":"success","options":[],"multiSelect":false,"placeholder":"Describe success..."}
    ])
}
impl Session {
    pub(super) async fn run(
        &self,
        t: &Turn,
        prompt: Value,
        options: Value,
        early: bool,
    ) -> Result<()> {
        let text = text(&prompt);
        if !early && matches("withhold synthetic user message until interrupted", &text) {
            t.cancel.cancelled().await;
            return Ok(());
        }
        if !early {
            if let Some(count) = number(
                r"emit (\d+) assistant messages before synthetic user message",
                &text,
                500,
            ) {
                self.event(t, json!({"type":"turn_started"})).await?;
                for index in 1..=count {
                    self.timeline(t,json!({"type":"assistant_message","text":format!("Synthetic pre-echo message {index}"),"messageId":format!("{}-{index}",t.message_id)})).await?;
                    tokio::task::yield_now().await;
                }
                self.user(t, &prompt, &options).await?;
                tokio::task::yield_now().await;
                return self.finish(t, Some(usage(0, 0)), false).await;
            }
            if let Some(delay) = number(r"delay synthetic user message by (\d+)ms", &text, 2000) {
                tokio::time::sleep(Duration::from_millis(delay)).await;
            }
            self.event(t, json!({"type":"turn_started"})).await?;
            self.user(t, &prompt, &options).await?;
        }
        if matches(r"emit\s+(?:a\s+)?synthetic\s+turn\s+failure", &text) {
            return self.finish(t, None, true).await;
        }
        if let Some(shape) = captures(
            r"replay a (claude|codex)-shaped foreground shell tool call",
            &text,
        )
        .filter(|c| matches!(c[1].as_str(), "claude" | "codex"))
        {
            if shape[1] == "codex" {
                self.assistant(t, "Running the foreground command.").await?;
            }
            let call = format!("{}:steering-replay-shell", t.id);
            let mut detail =
                json!({"type":"shell","command":"sleep 5","cwd":"/tmp/paseo-mock-load"});
            self.timeline(t, tool(call.clone(), "bash", "running", detail.clone()))
                .await?;
            tokio::time::sleep(Duration::from_millis(5000)).await;
            detail["output"] = json!("");
            detail["exitCode"] = json!(0);
            self.timeline(t, tool(call, "bash", "completed", detail))
                .await?;
            self.assistant(t, "Foreground command completed after steering.")
                .await?;
            return self.finish(t, Some(usage(0, 0)), false).await;
        }
        if let Some(response) = self.inner.features["mockStreamingAssistantResponse"].as_str() {
            let interval = self.inner.features["mockStreamingAssistantIntervalMs"]
                .as_f64()
                .filter(|v| v.is_finite() && *v >= 1.0)
                .unwrap_or(40.0)
                .min(1000.0) as u64;
            let tokens = tokenize(response);
            let count = tokens.len() as u64;
            for token in tokens {
                self.assistant(t, &token).await?;
                tokio::time::sleep(Duration::from_millis(interval)).await;
            }
            return self.finish(t, Some(usage(0, count)), false).await;
        }
        let configured = self
            .inner
            .state
            .lock()
            .unwrap()
            .responses
            .pop_front()
            .or_else(|| {
                self.inner.features["mockAssistantResponse"]
                    .as_str()
                    .map(str::to_owned)
            });
        let settled = configured.or_else(|| structured(&text)).or_else(|| {
            captures(
                r"^emit settled assistant image markdown:\s*(!\[[^\]\r\n]*\]\(.+\))\s*$",
                &text,
            )
            .map(|c| c[1].clone())
        });
        if let Some(response) = settled {
            self.assistant(t, &response).await?;
            return self.finish(t, None, false).await;
        }
        let plan = matches(r"emit\s+(?:a\s+)?synthetic\s+plan\s+approval", &text);
        if plan || matches(r"emit\s+(?:a\s+)?synthetic\s+questions?", &text) {
            let request = if plan {
                json!({"id":format!("mock-plan-{}",t.id),"provider":"mock","name":"MockPlanApproval","kind":"plan","title":"Plan","description":"Review the proposed plan before implementation starts.","input":{"plan":"1. Add the (c) README note.\n2. Run --name=\"my repo\".\n3. Verify ---buzz in the diff."},"actions":[{"id":"implement","label":"Implement","behavior":"allow","variant":"primary","intent":"implement"},{"id":"dismiss","label":"Dismiss","behavior":"deny","variant":"secondary","intent":"dismiss"}],"metadata":{"source":"mock_plan_approval"}})
            } else {
                json!({"id":format!("mock-questions-{}",t.id),"provider":"mock","name":"MockQuestions","kind":"question","title":"Questions","input":{"questions":questions(&text)},"metadata":{"source":"mock_questions"}})
            };
            let _gate = self.inner.gate.lock().await;
            if !self.active(t) {
                return Ok(());
            }
            self.inner
                .state
                .lock()
                .unwrap()
                .permissions
                .push(request.clone());
            return self.emit_locked(json!({"type":"permission_requested","provider":"mock","turnId":t.id,"request":request})).await;
        }
        if let Some(c) = captures(
            r"emit\s+(\d+)\s+(?:byte\s+)?(?:large\s+)?(diff|file|image)\s+agent stream (?:update|payload)",
            &text,
        ) {
            let bytes = c[1].parse::<f64>().unwrap_or(0.0);
            if bytes.is_finite() && bytes > 0.0 {
                let bytes = bytes.min(1_000_000.0) as usize;
                let kind = c[2].to_lowercase();
                let line = format!("{kind} {}\n", "x".repeat(96));
                let payload = line.repeat(bytes.div_ceil(line.len()))[..bytes].to_owned();
                match kind.as_str() {
                    "diff"=>self.timeline(t,tool(format!("{}:edit:large",t.id),"edit","completed",json!({"type":"edit","filePath":"src/large-diff.ts","unifiedDiff":format!("diff --git a/src/large-diff.ts b/src/large-diff.ts\n{payload}")}))).await?,
                    "file"=>self.timeline(t,tool(format!("{}:read:large",t.id),"read","completed",json!({"type":"read","filePath":"src/large-file.txt","content":payload}))).await?,
                    _=>{let mut image=base64::engine::general_purpose::STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAusB9Wl4Kj8AAAAASUVORK5CYII=")?;image.resize(bytes.max(image.len()),0);self.assistant(t,&format!("![Synthetic image](data:image/png;base64,{})",base64::engine::general_purpose::STANDARD.encode(image))).await?;}
                }
                return self
                    .finish(t, Some(payload_usage(bytes as u64)), false)
                    .await;
            }
        }
        if let Some(c) = captures(r"emit\s+(\d+)\s+(coalesced\s+)?agent stream updates", &text) {
            let count = c[1].parse::<f64>().unwrap_or(0.0);
            if count.is_finite() && count > 0.0 {
                let count = count.min(5000.0) as u64;
                for i in 0..count {
                    if c[2].is_empty() {
                        self.timeline(t,json!({"type":"todo","items":[{"text":format!("stress-update-{i}"),"completed":i%2==0}]})).await?;
                    } else {
                        self.assistant(t, &format!("stress-update-{i}")).await?;
                    }
                }
                return self.finish(t, Some(payload_usage(count)), false).await;
            }
        }
        self.stream(t).await
    }
    async fn stream(&self, t: &Turn) -> Result<()> {
        let burst = t.profile["burstMinTokens"].is_number();
        let interval = if burst {
            90
        } else {
            t.profile["intervalMs"].as_u64().unwrap_or(40)
        };
        let duration = Duration::from_millis(t.profile["durationMs"].as_u64().unwrap_or(300000));
        let mut queue = VecDeque::new();
        let mut cycle = 0;
        let mut tokens = 0;
        let mut burst_index = 0;
        loop {
            if t.started.elapsed() >= duration {
                self.assistant(t, "\n\n_(end of synthetic stream)_\n")
                    .await?;
                return self.finish(t, Some(usage(cycle, tokens)), false).await;
            }
            let count = if burst { burst_size(burst_index) } else { 1 };
            burst_index += 1;
            for _ in 0..count {
                if queue.is_empty() {
                    cycle += 1;
                    queue = cycle_queue(t, cycle, burst);
                }
                let Some(mut item) = queue.pop_front() else {
                    break;
                };
                if item["type"] == "usage" {
                    self.event(
                        t,
                        json!({"type":"usage_updated","usage":usage(cycle,tokens)}),
                    )
                    .await?;
                } else {
                    if item["type"] == "assistant_message" {
                        tokens += 1;
                        item["messageId"] = json!(t.message_id);
                    } else if item["type"] == "reasoning" {
                        tokens += 1;
                    }
                    self.timeline(t, item).await?;
                }
            }
            tokio::time::sleep(Duration::from_millis(interval)).await;
        }
    }
}
fn cycle_queue(t: &Turn, cycle: u64, burst: bool) -> VecDeque<Value> {
    let intro = FIXTURES["IntroParagraph"]
        .as_str()
        .unwrap()
        .replace("${cycle}", &cycle.to_string());
    let mid = FIXTURES["MidParagraph"].as_str().unwrap();
    let closing = FIXTURES["ClosingParagraph"].as_str().unwrap();
    let mut q = VecDeque::new();
    fn push(q: &mut VecDeque<Value>, kind: &str, text: &str) {
        q.extend(
            tokenize(text)
                .into_iter()
                .map(|text| json!({"type":kind,"text":text})),
        );
    }
    if burst {
        push(
            &mut q,
            "assistant_message",
            &format!("{intro}\n\n{mid}\n\n{closing}"),
        );
        return q;
    }
    push(&mut q, "assistant_message", &intro);
    push(
        &mut q,
        "reasoning",
        FIXTURES["ReasoningText"].as_str().unwrap(),
    );
    let read =
        json!({"type":"read","filePath":"packages/app/src/components/conversation-list.tsx"});
    let mut done = read.clone();
    done["content"] = json!(
        "export function ConversationList() {\n  const ref = useRef<FlatList>(null);\n  // ...\n}"
    );
    q.push_back(tool(
        format!("{}:read:{cycle}", t.id),
        "read",
        "running",
        read,
    ));
    q.push_back(tool(
        format!("{}:read:{cycle}", t.id),
        "read",
        "completed",
        done,
    ));
    let grep = json!({"type":"search","query":"scrollToEnd","toolName":"grep","mode":"files_with_matches"});
    let mut done = grep.clone();
    done["filePaths"] = json!([
        "packages/app/src/components/conversation-list.tsx",
        "packages/app/src/hooks/use-scroll-anchor.ts"
    ]);
    done["numFiles"] = json!(2);
    done["numMatches"] = json!(5);
    q.push_back(tool(
        format!("{}:grep:{cycle}", t.id),
        "grep",
        "running",
        grep,
    ));
    q.push_back(tool(
        format!("{}:grep:{cycle}", t.id),
        "grep",
        "completed",
        done,
    ));
    push(&mut q, "assistant_message", mid);
    let file = "packages/app/src/hooks/use-scroll-anchor.ts";
    let edit = json!({"type":"edit","filePath":file,"oldString":"const NEAR_BOTTOM_PX = 80;","newString":"const NEAR_BOTTOM_PX = 160;","unifiedDiff":FIXTURES["EditDiff"].as_str().unwrap().replace("${filePath}",file)});
    let shell = json!({"type":"shell","command":"node scripts/simulate-stream-burst.mjs","cwd":"/tmp/paseo-mock-load","output":"[burst] tick 1 userIsAtBottom=true\n[burst] tick 2 userIsAtBottom=true\n[burst] drag-start isDragging=true\n[burst] tick 3 suppressed\n[burst] drag-end isDragging=false\n","exitCode":0});
    for (name, detail) in [("edit", edit), ("bash", shell)] {
        for status in ["running", "completed"] {
            q.push_back(tool(
                format!("{}:{name}:{cycle}", t.id),
                name,
                status,
                detail.clone(),
            ));
        }
    }
    push(&mut q, "assistant_message", closing);
    q.push_back(json!({"type":"usage"}));
    q
}
