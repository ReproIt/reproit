//! The agentic tool loop: call, execute requested tools, feed results back,
//! repeat until the model stops asking.

use super::types::*;
use super::{Client, Error, Result};
use serde_json::Value;
use std::future::Future;

pub const MAX_TOOL_ITERATIONS: usize = 50;

/// One tool execution. Err is reported to the model with is_error (the model
/// adapts); it does not abort the loop.
pub type ToolOutcome = std::result::Result<String, String>;

impl Client {
    pub async fn run_tool_loop<F, Fut>(
        &self,
        mut req: MessagesRequest,
        mut exec: F,
    ) -> Result<MessagesResponse>
    where
        F: FnMut(String, Value) -> Fut,
        Fut: Future<Output = ToolOutcome>,
    {
        for _ in 0..MAX_TOOL_ITERATIONS {
            let resp = self.messages(&req).await?;
            match resp.stop_reason.as_deref() {
                Some("tool_use") => {
                    let mut results: Vec<ContentBlock> = Vec::new();
                    for (id, name, input) in resp.tool_uses() {
                        let outcome = exec(name.to_string(), input.clone()).await;
                        let (content, is_error) = match outcome {
                            Ok(s) => (Value::String(s), false),
                            Err(s) => (Value::String(s), true),
                        };
                        results.push(ContentBlock::ToolResult {
                            tool_use_id: id.to_string(),
                            content,
                            is_error,
                        });
                    }
                    req.messages.push(MessageParam {
                        role: Role::Assistant,
                        content: Content::Blocks(resp.content.clone()),
                    });
                    req.messages.push(MessageParam {
                        role: Role::User,
                        content: Content::Blocks(results),
                    });
                }
                // Server-side tool loop paused; re-send to resume. No extra
                // user message: the API detects the trailing state itself.
                Some("pause_turn") => {
                    req.messages.push(MessageParam {
                        role: Role::Assistant,
                        content: Content::Blocks(resp.content.clone()),
                    });
                }
                _ => return Ok(resp),
            }
        }
        Err(Error::ToolLoopOverrun(MAX_TOOL_ITERATIONS))
    }
}
