use super::Timeline;
use anyhow::Result;
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use serde_json::{Value, json};

fn searchable_blocks(text: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut block = String::new();
    let mut image_depth: usize = 0;
    // markdown-it enables tables/strikethrough but disables raw HTML. HTML parser
    // events are consequently visible text here rather than executable markup.
    let options = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH;
    let flush = |block: &mut String, blocks: &mut Vec<String>| {
        if !block.is_empty() {
            blocks.push(std::mem::take(block));
        }
    };
    for event in Parser::new_ext(text, options) {
        match event {
            Event::Start(Tag::Image { .. }) => image_depth += 1,
            Event::End(TagEnd::Image) => image_depth = image_depth.saturating_sub(1),
            _ if image_depth > 0 => {}
            Event::Start(
                Tag::Paragraph
                | Tag::Heading { .. }
                | Tag::CodeBlock(_)
                | Tag::Item
                | Tag::TableCell,
            ) => flush(&mut block, &mut blocks),
            Event::End(
                TagEnd::Paragraph
                | TagEnd::Heading(_)
                | TagEnd::CodeBlock
                | TagEnd::Item
                | TagEnd::TableCell,
            ) => flush(&mut block, &mut blocks),
            Event::Text(text) | Event::Code(text) | Event::Html(text) | Event::InlineHtml(text) => {
                block.push_str(&text)
            }
            Event::SoftBreak | Event::HardBreak => block.push('\n'),
            Event::Rule => flush(&mut block, &mut blocks),
            _ => {}
        }
    }
    flush(&mut block, &mut blocks);
    blocks
}
impl Timeline {
    pub fn search(&self, options: &Value) -> Result<Value> {
        let query = options["query"].as_str().unwrap_or("").trim();
        let mut locations = Vec::new();
        if query.is_empty() {
            return Ok(json!({"epoch":self.epoch,"locations":locations,"nextCursor":null}));
        }
        let pattern = regex::RegexBuilder::new(
            &query
                .split_whitespace()
                .map(regex::escape)
                .collect::<Vec<_>>()
                .join("\\s+"),
        )
        .case_insensitive(true)
        .build()?;
        let cursor = options["cursor"].as_i64().unwrap_or(0);
        for row in &self.rows {
            let seq = row["seqEnd"].as_i64().unwrap_or(0);
            if seq <= cursor {
                continue;
            }
            let role = match row["item"]["type"].as_str() {
                Some("user_message") => "user",
                Some("assistant_message") => "assistant",
                _ => continue,
            };
            let raw = row["item"]["text"].as_str().unwrap_or("").replace('\r', "");
            let blocks = if role == "assistant" {
                searchable_blocks(&raw)
            } else {
                vec![raw]
            };
            let count = blocks
                .iter()
                .map(|block| pattern.find_iter(block).count())
                .sum::<usize>();
            if count == 0 {
                continue;
            }
            if locations.len() == 200 {
                let next = locations.last().and_then(|v: &Value| v.get("seq")).cloned();
                return Ok(json!({"epoch":self.epoch,"locations":locations,"nextCursor":next}));
            }
            locations.push(json!({"seq":seq,"role":role,"count":count}));
        }
        Ok(json!({"epoch":self.epoch,"locations":locations,"nextCursor":null}))
    }
    pub fn prompts(&self) -> Value {
        let prompts: Vec<Value> = self
            .rows
            .iter()
            .filter(|row| row["item"]["type"] == "user_message")
            .map(|row| {
                let preview = row["item"]["text"]
                    .as_str()
                    .unwrap_or("")
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ");
                let preview = if preview.encode_utf16().count() > 120 {
                    crate::activity::utf16_prefix(&preview, 119) + "…"
                } else {
                    preview
                };
                json!({"seq":row["seq"],"timestamp":row["timestamp"],"preview":preview})
            })
            .collect();
        json!({"epoch":self.epoch,"prompts":prompts})
    }
}
