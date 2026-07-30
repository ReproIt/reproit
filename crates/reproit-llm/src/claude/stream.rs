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
        drain_events(&mut buf, &mut acc, on_text)?;
    }
    acc.finish()
}

/// Consume every complete line buffered so far: `data:` payloads feed the
/// accumulator; anything else (event names, comments, blanks, malformed
/// JSON) is skipped. A chunk may end mid-line, so the partial tail stays in
/// `buf` for the next chunk.
fn drain_events(buf: &mut String, acc: &mut Acc, on_text: &mut dyn FnMut(&str)) -> Result<()> {
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
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Feed raw SSE text through the accumulator in `chunk`-byte pieces,
    /// exercising the same partial-line buffering `accumulate` performs on
    /// network chunks. Returns the folded response and the text-delta trace.
    fn feed(raw: &str, chunk: usize) -> Result<(MessagesResponse, String)> {
        let mut acc = Acc::default();
        let mut buf = String::new();
        let mut seen = String::new();
        let mut on_text = |t: &str| seen.push_str(t);
        for piece in raw.as_bytes().chunks(chunk) {
            buf.push_str(&String::from_utf8_lossy(piece));
            drain_events(&mut buf, &mut acc, &mut on_text)?;
        }
        Ok((acc.finish()?, seen))
    }

    /// A full streamed message: text deltas, a tool-use input assembled from
    /// partial JSON, and the closing message_delta.
    const HAPPY: &str = concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",",
        "\"model\":\"claude-opus-4-8\",",
        "\"usage\":{\"input_tokens\":7,\"output_tokens\":1}}}\n",
        "\n",
        "data: {\"type\":\"content_block_start\",\"index\":0,",
        "\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,",
        "\"delta\":{\"type\":\"text_delta\",\"text\":\"Hel\"}}\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,",
        "\"delta\":{\"type\":\"text_delta\",\"text\":\"lo\"}}\n",
        "data: {\"type\":\"content_block_stop\",\"index\":0}\n",
        "data: {\"type\":\"content_block_start\",\"index\":1,",
        "\"content_block\":{\"type\":\"tool_use\",\"id\":\"tu_1\",",
        "\"name\":\"run\",\"input\":{}}}\n",
        "data: {\"type\":\"content_block_delta\",\"index\":1,",
        "\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"cmd\\\":\"}}\n",
        "data: {\"type\":\"content_block_delta\",\"index\":1,",
        "\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"\\\"ls\\\"}\"}}\n",
        "data: {\"type\":\"content_block_stop\",\"index\":1}\n",
        "data: {\"type\":\"message_delta\",",
        "\"delta\":{\"stop_reason\":\"end_turn\",\"stop_details\":null},",
        "\"usage\":{\"output_tokens\":42}}\n",
    );

    fn assert_happy(resp: &MessagesResponse, seen: &str) {
        assert_eq!(resp.id, "msg_1");
        assert_eq!(resp.model, "claude-opus-4-8");
        assert_eq!(resp.text(), "Hello");
        assert_eq!(seen, "Hello");
        let ContentBlock::ToolUse { id, name, input } = &resp.content[1] else {
            panic!("expected a tool_use block, got {:?}", resp.content[1]);
        };
        assert_eq!(id, "tu_1");
        assert_eq!(name, "run");
        assert_eq!(input, &json!({"cmd": "ls"}));
        assert_eq!(resp.stop_reason.as_deref(), Some("end_turn"));
        assert!(resp.stop_details.is_none(), "null stop_details is dropped");
        assert_eq!(resp.usage.input_tokens, 7);
        assert_eq!(resp.usage.output_tokens, 42);
    }

    #[test]
    fn whole_stream_folds_into_one_response() {
        let (resp, seen) = feed(HAPPY, HAPPY.len()).unwrap();
        assert_happy(&resp, &seen);
    }

    #[test]
    fn chunk_boundaries_mid_line_do_not_change_the_result() {
        for chunk in [1, 7, 64] {
            let (resp, seen) = feed(HAPPY, chunk).unwrap();
            assert_happy(&resp, &seen);
        }
    }

    #[test]
    fn thinking_deltas_accumulate_and_keep_block_extras() {
        let raw = concat!(
            "data: {\"type\":\"content_block_start\",\"index\":0,",
            "\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\",",
            "\"signature\":\"sig-1\"}}\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,",
            "\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"first \"}}\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,",
            "\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"second\"}}\n",
        );
        let (resp, seen) = feed(raw, raw.len()).unwrap();
        assert_eq!(seen, "", "thinking deltas never reach on_text");
        let ContentBlock::Thinking { thinking, extra } = &resp.content[0] else {
            panic!("expected a thinking block, got {:?}", resp.content[0]);
        };
        assert_eq!(thinking, "first second");
        assert_eq!(extra["signature"], "sig-1");
    }

    #[test]
    fn noise_lines_are_skipped_without_derailing_the_stream() {
        let raw = concat!(
            ": keep-alive comment\n",
            "event: content_block_start\n",
            "data:\n",
            "data: {not json at all\n",
            "data: {\"type\":\"unknown_future_event\",\"index\":9}\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,",
            "\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,",
            "\"delta\":{\"type\":\"text_delta\",\"text\":\"ok\"}}\n",
        );
        let (resp, seen) = feed(raw, raw.len()).unwrap();
        assert_eq!(resp.text(), "ok");
        assert_eq!(seen, "ok");
    }

    #[test]
    fn error_event_becomes_a_stream_error() {
        let raw = concat!(
            "data: {\"type\":\"error\",",
            "\"error\":{\"type\":\"overloaded_error\",\"message\":\"overloaded\"}}\n",
        );
        let err = feed(raw, raw.len()).unwrap_err();
        match err {
            Error::Stream(msg) => assert_eq!(msg, "overloaded"),
            other => panic!("expected Error::Stream, got {other:?}"),
        }
    }

    #[test]
    fn gap_indices_are_filled_with_null_blocks() {
        let raw = concat!(
            "data: {\"type\":\"content_block_start\",\"index\":2,",
            "\"content_block\":{\"type\":\"text\",\"text\":\"tail\"}}\n",
        );
        let (resp, _) = feed(raw, raw.len()).unwrap();
        assert_eq!(resp.content.len(), 3);
        assert!(matches!(resp.content[0], ContentBlock::Other(Value::Null)));
        assert!(matches!(resp.content[1], ContentBlock::Other(Value::Null)));
        assert_eq!(resp.text(), "tail");
    }

    #[test]
    fn empty_partial_json_leaves_the_start_input_untouched() {
        let raw = concat!(
            "data: {\"type\":\"content_block_start\",\"index\":0,",
            "\"content_block\":{\"type\":\"tool_use\",\"id\":\"tu_2\",",
            "\"name\":\"noop\",\"input\":{}}}\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,",
            "\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"\"}}\n",
            "data: {\"type\":\"content_block_stop\",\"index\":0}\n",
        );
        let (resp, _) = feed(raw, raw.len()).unwrap();
        let ContentBlock::ToolUse { input, .. } = &resp.content[0] else {
            panic!("expected a tool_use block, got {:?}", resp.content[0]);
        };
        assert_eq!(input, &json!({}));
    }

    #[test]
    fn truncated_tool_input_is_a_json_error_not_a_panic() {
        let raw = concat!(
            "data: {\"type\":\"content_block_start\",\"index\":0,",
            "\"content_block\":{\"type\":\"tool_use\",\"id\":\"tu_3\",",
            "\"name\":\"run\",\"input\":{}}}\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,",
            "\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"cmd\\\":\"}}\n",
            "data: {\"type\":\"content_block_stop\",\"index\":0}\n",
        );
        let err = feed(raw, raw.len()).unwrap_err();
        assert!(matches!(err, Error::Json(_)), "got {err:?}");
    }
}
