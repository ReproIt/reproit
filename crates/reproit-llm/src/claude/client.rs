//! HTTP transport: auth, headers, retries. The only place in the workspace
//! that knows the Anthropic wire protocol.

use super::stream;
use super::types::*;
use super::{Error, Result};
use serde_json::Value;
use std::time::Duration;

const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
const ANTHROPIC_VERSION: &str = "2023-06-01";
const OAUTH_BETA: &str = "oauth-2025-04-20";

enum Auth {
    ApiKey(String),
    Bearer(String),
}

pub struct Client {
    http: reqwest::Client,
    base_url: String,
    auth: Auth,
}

impl Client {
    /// Credentials from the environment: ANTHROPIC_API_KEY first, then
    /// ANTHROPIC_AUTH_TOKEN (OAuth bearer, e.g. from `ant auth
    /// print-credentials`). Base URL override via ANTHROPIC_BASE_URL.
    pub fn from_env() -> Result<Self> {
        let nonempty =
            |v: std::result::Result<String, std::env::VarError>| v.ok().filter(|s| !s.is_empty());
        let auth = if let Some(k) = nonempty(std::env::var("ANTHROPIC_API_KEY")) {
            Auth::ApiKey(k)
        } else if let Some(t) = nonempty(std::env::var("ANTHROPIC_AUTH_TOKEN")) {
            Auth::Bearer(t)
        } else {
            return Err(Error::MissingCredentials);
        };
        let base_url = nonempty(std::env::var("ANTHROPIC_BASE_URL"))
            .unwrap_or_else(|| DEFAULT_BASE_URL.to_string());
        // Generous timeout: single hard requests can run many minutes.
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(900))
            .build()?;
        Ok(Client {
            http,
            base_url,
            auth,
        })
    }

    fn request(&self, body: &MessagesRequest) -> reqwest::RequestBuilder {
        let rb = self
            .http
            .post(format!("{}/v1/messages", self.base_url))
            .header("anthropic-version", ANTHROPIC_VERSION)
            .json(body);
        match &self.auth {
            Auth::ApiKey(k) => rb.header("x-api-key", k),
            // OAuth tokens go on Authorization: Bearer plus the oauth beta.
            Auth::Bearer(t) => rb.bearer_auth(t).header("anthropic-beta", OAUTH_BETA),
        }
    }

    /// Streaming request accumulated into a complete response. Required for
    /// long outputs (generated test files easily exceed non-streaming
    /// timeouts). `on_text` receives text deltas as they arrive; pass a
    /// no-op closure if you only want the final message.
    pub async fn messages_stream(
        &self,
        req: &MessagesRequest,
        on_text: &mut (dyn FnMut(&str) + Send),
    ) -> Result<MessagesResponse> {
        let mut req = req.clone();
        req.stream = Some(true);
        let resp = self.request(&req).send().await?;
        let status = resp.status();
        if !status.is_success() {
            let body: Value = resp.json().await.unwrap_or(Value::Null);
            return Err(api_error(status.as_u16(), &body));
        }
        stream::accumulate(resp, on_text).await
    }
}

fn api_error(status: u16, body: &Value) -> Error {
    Error::Api {
        status,
        error_type: body
            .pointer("/error/type")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string(),
        message: body
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        request_id: body
            .get("request_id")
            .and_then(Value::as_str)
            .map(String::from),
    }
}
