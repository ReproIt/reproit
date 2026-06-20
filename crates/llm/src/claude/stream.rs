//! SSE accumulator: folds a streaming response into one MessagesResponse.
//!
//! Caveat: thinking-block signature deltas are not reassembled, so blocks
//! from a streamed response are not suitable for byte-exact replay into a
//! follow-up turn. The non-streaming path preserves them verbatim; use that
//! for multi-turn tool loops.

use super::types::*;
use super::{Error, Result};
use futures_util::StreamExt;
use serde_json::Value;
use std::collections::HashMap;

pub(crate) async fn accumulate(
    resp: reqwest::Response,
    on_text: &mut (dyn FnMut(&str) + Send),
) -> Result<MessagesResponse> {
    let mut acc = Acc::default();
    let mut buf = String::new();
    let mut bytes = resp.bytes_stream();
    while let Some(chunk) = bytes.next().await {
        let chunk = chunk?;
        buf.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(pos) = buf.find('\n') {
            let line: String = buf.drain(..=pos).collect();
            let line = line.trim_end();
            let Some(data) = line.strip_prefix("data:") else {
                continue;
            };
            let data = data.trim();
            if data.is_empty() {
                continue;
            }
            let Ok(event) = serde_json::from_str::<Value>(data) else {
                continue;
            };
            acc.handle(&event, on_text)?;
        }
    }
    acc.finish()
}

#[derive(Default)]
struct Acc {
    id: String,
    model: String,
    content: Vec<ContentBlock>,
    stop_reason: Option<String>,
    stop_details: Option<Value>,
    usage: Usage,
    /// Tool-use inputs arrive as partial JSON strings, parsed at block stop.
    partial_json: HashMap<usize, String>,
}

impl Acc {
    fn handle(&mut self, event: &Value, on_text: &mut dyn FnMut(&str)) -> Result<()> {
        match event.get("type").and_then(Value::as_str).unwrap_or("") {
            "message_start" => {
                if let Some(msg) = event.get("message") {
                    self.id = msg
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    self.model = msg
                        .get("model")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    if let Some(u) = msg.get("usage") {
                        if let Ok(usage) = serde_json::from_value::<Usage>(u.clone()) {
                            self.usage = usage;
                        }
                    }
                }
            }
            "content_block_start" => {
                let idx = event.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                if let Some(block) = event.get("content_block") {
                    let parsed: ContentBlock = serde_json::from_value(block.clone())?;
                    while self.content.len() < idx {
                        self.content.push(ContentBlock::Other(Value::Null));
                    }
                    self.content.push(parsed);
                }
            }
            "content_block_delta" => {
                let idx = event.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                let Some(delta) = event.get("delta") else {
                    return Ok(());
                };
                match delta.get("type").and_then(Value::as_str).unwrap_or("") {
                    "text_delta" => {
                        let t = delta.get("text").and_then(Value::as_str).unwrap_or("");
                        on_text(t);
                        if let Some(ContentBlock::Text { text }) = self.content.get_mut(idx) {
                            text.push_str(t);
                        }
                    }
                    "thinking_delta" => {
                        let t = delta.get("thinking").and_then(Value::as_str).unwrap_or("");
                        if let Some(ContentBlock::Thinking { thinking, .. }) =
                            self.content.get_mut(idx)
                        {
                            thinking.push_str(t);
                        }
                    }
                    "input_json_delta" => {
                        let t = delta
                            .get("partial_json")
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        self.partial_json.entry(idx).or_default().push_str(t);
                    }
                    _ => {}
                }
            }
            "content_block_stop" => {
                let idx = event.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                if let Some(json) = self.partial_json.remove(&idx) {
                    if let Some(ContentBlock::ToolUse { input, .. }) = self.content.get_mut(idx) {
                        if !json.is_empty() {
                            *input = serde_json::from_str(&json)?;
                        }
                    }
                }
            }
            "message_delta" => {
                if let Some(delta) = event.get("delta") {
                    if let Some(sr) = delta.get("stop_reason").and_then(Value::as_str) {
                        self.stop_reason = Some(sr.to_string());
                    }
                    if let Some(sd) = delta.get("stop_details") {
                        if !sd.is_null() {
                            self.stop_details = Some(sd.clone());
                        }
                    }
                }
                if let Some(u) = event.get("usage") {
                    if let Some(out) = u.get("output_tokens").and_then(Value::as_u64) {
                        self.usage.output_tokens = out;
                    }
                }
            }
            "error" => {
                let msg = event
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown stream error");
                return Err(Error::Stream(msg.to_string()));
            }
            _ => {}
        }
        Ok(())
    }

    fn finish(self) -> Result<MessagesResponse> {
        Ok(MessagesResponse {
            id: self.id,
            model: self.model,
            content: self.content,
            stop_reason: self.stop_reason,
            stop_details: self.stop_details,
            usage: self.usage,
        })
    }
}
