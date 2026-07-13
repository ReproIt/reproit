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
const MAX_ATTEMPTS: u32 = 4;

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

    /// Non-streaming request with retry on 429/5xx/529 (exponential backoff,
    /// honoring retry-after). Keep max_tokens at or under ~16k here; stream
    /// above that.
    pub async fn messages(&self, req: &MessagesRequest) -> Result<MessagesResponse> {
        let mut req = req.clone();
        req.stream = None;
        let mut attempt = 0u32;
        loop {
            attempt += 1;
            match self.request(&req).send().await {
                Ok(resp) => {
                    let status = resp.status();
                    if status.is_success() {
                        return Ok(resp.json::<MessagesResponse>().await?);
                    }
                    let retryable = matches!(status.as_u16(), 429 | 500 | 502 | 503 | 529);
                    let retry_after = resp
                        .headers()
                        .get("retry-after")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|s| s.parse::<u64>().ok());
                    let body: Value = resp.json().await.unwrap_or(Value::Null);
                    if retryable && attempt < MAX_ATTEMPTS {
                        let delay = retry_after.unwrap_or_else(|| 2u64.pow(attempt - 1));
                        tokio::time::sleep(Duration::from_secs(delay)).await;
                        continue;
                    }
                    return Err(api_error(status.as_u16(), &body));
                }
                Err(e) => {
                    if attempt < MAX_ATTEMPTS && (e.is_connect() || e.is_timeout()) {
                        tokio::time::sleep(Duration::from_secs(2u64.pow(attempt - 1))).await;
                        continue;
                    }
                    return Err(e.into());
                }
            }
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
