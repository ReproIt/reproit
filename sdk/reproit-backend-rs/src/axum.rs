//! First-class axum 0.8 middleware (tower `Layer`), behind the `axum`
//! feature.
//!
//! ```ignore
//! let layer = reproit_backend::axum::ReproitLayer::new(MiddlewareConfig {
//!     capture,
//!     ..MiddlewareConfig::default()
//! });
//! let app = Router::new().route(..).layer(layer);
//! ```
//!
//! Handlers fetch the recorder from the request extensions
//! (`Extension<Recorder>` or `request.extensions().get::<Recorder>()`) and
//! record observed effects with `recorder.effect(..)`.

use crate::framework::{
    decode_json, multi_map, query_pairs, resolve_context, PendingTrace, Recorder, MAX_BODY_BYTES,
};
use crate::{BackendTrace, Capture, HttpInput};
use ::axum::body::{to_bytes, Body};
use ::axum::http::header::CONTENT_TYPE;
use ::axum::http::request::Parts;
use ::axum::http::{HeaderValue, Request};
use ::axum::response::Response;
use http_body::Body as _;
use serde_json::Value;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

type OperationFn = dyn Fn(&Parts) -> String + Send + Sync;
type TenantFn = dyn Fn(&Parts) -> Option<String> + Send + Sync;

/// Middleware configuration. The default is a scan-time-only adapter with
/// `METHOD /path` operation names.
#[derive(Default)]
pub struct MiddlewareConfig {
    /// Enables production capture mode; `None` keeps scan-time only.
    pub capture: Option<Capture>,
    /// Names the traced operation; default `METHOD /path`.
    pub operation: Option<Box<OperationFn>>,
    /// Extracts a non-secret tenant identifier; default none.
    pub tenant: Option<Box<TenantFn>>,
    /// Asserts the adapter observed every persistent effect.
    pub effects_complete: bool,
}

/// Tower layer wrapping every request in the backend trace adapter.
#[derive(Clone)]
pub struct ReproitLayer {
    config: Arc<MiddlewareConfig>,
}

impl ReproitLayer {
    pub fn new(config: MiddlewareConfig) -> Self {
        Self {
            config: Arc::new(config),
        }
    }
}

impl<S> tower_layer::Layer<S> for ReproitLayer {
    type Service = ReproitService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        ReproitService {
            inner,
            config: self.config.clone(),
        }
    }
}

/// The layered service; not constructed directly.
#[derive(Clone)]
pub struct ReproitService<S> {
    inner: S,
    config: Arc<MiddlewareConfig>,
}

impl<S> tower_service::Service<Request<Body>> for ReproitService<S>
where
    S: tower_service::Service<Request<Body>, Response = Response> + Clone + Send + 'static,
    S::Future: Send,
{
    type Response = Response;
    type Error = S::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Response, S::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), S::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, request: Request<Body>) -> Self::Future {
        let clone = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, clone);
        let config = self.config.clone();
        Box::pin(async move {
            let (request, pending) = begin(request, &config).await;
            let response = inner.call(request).await?;
            match pending {
                None => Ok(response),
                Some(pending) => Ok(finish(response, pending, &config).await),
            }
        })
    }
}

/// Begin the trace (fail closed: `None` leaves the request untraced) and
/// hand the possibly re-buffered request back.
async fn begin(
    request: Request<Body>,
    config: &MiddlewareConfig,
) -> (Request<Body>, Option<PendingTrace>) {
    let (mut parts, body) = request.into_parts();
    let get_header = |name: &str| {
        parts
            .headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
    };
    let Some((context, scan)) = resolve_context(get_header, config.capture.as_ref()) else {
        return (Request::from_parts(parts, body), None);
    };
    let content_type = parts
        .headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_string();
    let (body, body_json) = buffer_body(body, &content_type).await;
    let operation = match &config.operation {
        Some(operation) => operation(&parts),
        None => format!("{} {}", parts.method, parts.uri.path()),
    };
    let tenant = config.tenant.as_ref().and_then(|tenant| tenant(&parts));
    let input = HttpInput {
        body: body_json,
        query: multi_map(query_pairs(parts.uri.query().unwrap_or(""))),
        headers: multi_map(parts.headers.iter().filter_map(|(name, value)| {
            Some((name.as_str().to_string(), value.to_str().ok()?.to_string()))
        })),
        ..HttpInput::default()
    };
    let pending = BackendTrace::begin(
        context,
        operation,
        None,
        tenant,
        None,
        input.into_value(),
        Vec::new(),
    )
    .ok()
    .map(|trace| {
        let recorder = Recorder::new(trace);
        parts.extensions.insert(recorder.clone());
        PendingTrace { recorder, scan }
    });
    (Request::from_parts(parts, body), pending)
}

/// Finish the trace once the handler has produced the response, attach the
/// scan header or hand the trace to the capture sampler, and ship.
async fn finish(response: Response, pending: PendingTrace, config: &MiddlewareConfig) -> Response {
    let Some(mut trace) = pending.recorder.take() else {
        return response;
    };
    let (mut parts, body) = response.into_parts();
    let content_type = parts
        .headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_string();
    let (body, output) = buffer_body(body, &content_type).await;
    let status = parts.status.as_u16();
    let finished = trace.finish(
        output.unwrap_or(Value::Null),
        status,
        status < 500,
        config.effects_complete,
    );
    if finished.is_ok() {
        if pending.scan {
            // Oversized or over-long traces drop their header; the
            // response ships regardless.
            if let Some(header) = trace
                .header()
                .ok()
                .and_then(|value| HeaderValue::from_str(&value).ok())
            {
                parts.headers.insert("x-reproit-events", header);
            }
        } else if let Some(capture) = &config.capture {
            capture.record(&trace);
        }
    }
    Response::from_parts(parts, body)
}

/// Buffer a body only when its exact size is known and within the cap:
/// streaming or oversized bodies pass through untouched (traced without
/// content), so the middleware never breaks or unboundedly holds a body.
async fn buffer_body(body: Body, content_type: &str) -> (Body, Option<Value>) {
    let exact = body.size_hint().exact();
    match exact {
        Some(size) if size as usize <= MAX_BODY_BYTES => {
            match to_bytes(body, MAX_BODY_BYTES).await {
                Ok(bytes) => {
                    let decoded = decode_json(&bytes, content_type);
                    (Body::from(bytes), decoded)
                }
                // The body failed mid-read; nothing left to restore.
                Err(_) => (Body::empty(), None),
            }
        }
        _ => (body, None),
    }
}
