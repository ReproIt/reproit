//! Messages API request/response types. Serialization matches the wire
//! format exactly; unknown content blocks survive as raw JSON.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Workspace-wide default. Change here, nowhere else.
pub const DEFAULT_MODEL: &str = "claude-opus-4-8";

#[derive(Debug, Clone, Serialize)]
pub struct MessagesRequest {
    pub model: String,
    pub max_tokens: u32,
    pub messages: Vec<MessageParam>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub tools: Vec<ToolDef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<Thinking>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_config: Option<OutputConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
}

impl MessagesRequest {
    pub fn new(max_tokens: u32) -> Self {
        MessagesRequest {
            model: DEFAULT_MODEL.to_string(),
            max_tokens,
            messages: Vec::new(),
            system: None,
            tools: Vec::new(),
            tool_choice: None,
            thinking: None,
            output_config: None,
            stream: None,
        }
    }

    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    pub fn system(mut self, system: impl Into<String>) -> Self {
        self.system = Some(system.into());
        self
    }

    pub fn user(mut self, text: impl Into<String>) -> Self {
        self.messages.push(MessageParam {
            role: Role::User,
            content: Content::Text(text.into()),
        });
        self
    }

    pub fn message(mut self, role: Role, content: Content) -> Self {
        self.messages.push(MessageParam { role, content });
        self
    }

    pub fn tools(mut self, tools: Vec<ToolDef>) -> Self {
        self.tools = tools;
        self
    }

    /// Adaptive thinking: the right default for anything complicated.
    pub fn thinking_adaptive(mut self) -> Self {
        self.thinking = Some(Thinking::Adaptive { display: None });
        self
    }

    /// "low" | "medium" | "high" | "xhigh" | "max"
    pub fn effort(mut self, effort: impl Into<String>) -> Self {
        self.output_config
            .get_or_insert_with(OutputConfig::default)
            .effort = Some(effort.into());
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageParam {
    pub role: Role,
    pub content: Content,
}

/// A message body: either a bare string or content blocks.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Content {
    Text(String),
    Blocks(Vec<ContentBlock>),
}

/// Content blocks we understand, plus a verbatim passthrough for everything
/// else (protected thinking, future block types). The passthrough is load
/// bearing: blocks echoed back to the API must be byte-identical.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    Thinking {
        #[serde(default)]
        thinking: String,
        #[serde(flatten)]
        extra: Value,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    ToolResult {
        tool_use_id: String,
        content: Value,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        is_error: bool,
    },
    #[serde(untagged)]
    Other(Value),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Thinking {
    Adaptive {
        #[serde(skip_serializing_if = "Option::is_none")]
        display: Option<String>,
    },
    Disabled,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct OutputConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MessagesResponse {
    pub id: String,
    pub model: String,
    pub content: Vec<ContentBlock>,
    pub stop_reason: Option<String>,
    #[serde(default)]
    pub stop_details: Option<Value>,
    pub usage: Usage,
}

impl MessagesResponse {
    /// All text blocks, concatenated. Check `is_refusal()` first.
    pub fn text(&self) -> String {
        let mut out = String::new();
        for block in &self.content {
            if let ContentBlock::Text { text } = block {
                out.push_str(text);
            }
        }
        out
    }

    pub fn tool_uses(&self) -> Vec<(&str, &str, &Value)> {
        self.content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::ToolUse { id, name, input } => {
                    Some((id.as_str(), name.as_str(), input))
                }
                _ => None,
            })
            .collect()
    }

    /// Safety classifiers declined (HTTP 200 with stop_reason "refusal").
    /// Always check before reading content.
    pub fn is_refusal(&self) -> bool {
        self.stop_reason.as_deref() == Some("refusal")
    }

    pub fn refusal_category(&self) -> Option<String> {
        self.stop_details
            .as_ref()
            .and_then(|d| d.get("category"))
            .and_then(|c| c.as_str())
            .map(String::from)
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_creation_input_tokens: Option<u64>,
    #[serde(default)]
    pub cache_read_input_tokens: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_serializes_minimal() {
        let req = MessagesRequest::new(1024)
            .user("hi")
            .thinking_adaptive()
            .effort("high");
        let v = serde_json::to_value(&req).unwrap();
        assert_eq!(v["model"], DEFAULT_MODEL);
        assert_eq!(v["thinking"]["type"], "adaptive");
        assert_eq!(v["output_config"]["effort"], "high");
        assert!(v.get("system").is_none());
        assert!(v.get("tools").is_none());
    }

    #[test]
    fn response_tolerates_unknown_blocks() {
        let raw = r#"{
            "id": "msg_1", "model": "claude-opus-4-8",
            "content": [
                {"type": "thinking", "thinking": "", "signature": "abc"},
                {"type": "text", "text": "hello"},
                {"type": "some_future_block", "payload": 42}
            ],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 10, "output_tokens": 5}
        }"#;
        let resp: MessagesResponse = serde_json::from_str(raw).unwrap();
        assert_eq!(resp.text(), "hello");
        assert!(matches!(resp.content[2], ContentBlock::Other(_)));
        // Unknown blocks round-trip verbatim.
        let echoed = serde_json::to_value(&resp.content[2]).unwrap();
        assert_eq!(echoed["type"], "some_future_block");
        assert_eq!(echoed["payload"], 42);
    }

    #[test]
    fn refusal_detected() {
        let raw = r#"{
            "id": "msg_2", "model": "claude-opus-4-8",
            "content": [],
            "stop_reason": "refusal",
            "stop_details": {"category": "cyber"},
            "usage": {}
        }"#;
        let resp: MessagesResponse = serde_json::from_str(raw).unwrap();
        assert!(resp.is_refusal());
        assert_eq!(resp.refusal_category().as_deref(), Some("cyber"));
    }
}
