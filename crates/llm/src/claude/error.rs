use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("missing credentials: set ANTHROPIC_API_KEY (or ANTHROPIC_AUTH_TOKEN)")]
    MissingCredentials,

    #[error("api error {status} ({error_type}): {message}")]
    Api {
        status: u16,
        error_type: String,
        message: String,
        request_id: Option<String>,
    },

    /// The model declined the request (HTTP 200, stop_reason "refusal").
    /// Surfaced as an error only by helpers that need text; the raw response
    /// is still available via `Client::messages`.
    #[error("model refused the request{}", category.as_deref().map(|c| format!(" (category: {c})")).unwrap_or_default())]
    Refusal { category: Option<String> },

    #[error("tool loop exceeded {0} iterations")]
    ToolLoopOverrun(usize),

    #[error("stream protocol error: {0}")]
    Stream(String),

    #[error(transparent)]
    Http(#[from] reqwest::Error),

    #[error(transparent)]
    Json(#[from] serde_json::Error),
}
