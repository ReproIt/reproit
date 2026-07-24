//! First-class actix-web 4 middleware (`Transform`), behind the `actix`
//! feature.
//!
//! ```ignore
//! let app = App::new()
//!     .wrap(reproit_backend::actix::Reproit::new(MiddlewareConfig {
//!         capture,
//!         ..MiddlewareConfig::default()
//!     }))
//!     .service(..);
//! ```
//!
//! Handlers fetch the recorder from the request extensions
//! (`web::ReqData<Recorder>` or `req.extensions().get::<Recorder>()`) and
//! record observed effects with `recorder.effect(..)`.

use crate::framework::{
    decode_json, multi_map, query_pairs, resolve_context, PendingTrace, Recorder, MAX_BODY_BYTES,
};
use crate::{BackendTrace, Capture, HttpInput};
use actix_web::body::{BodySize, BoxBody, MessageBody};
use actix_web::dev::{Payload, Service, ServiceRequest, ServiceResponse, Transform};
use actix_web::error::PayloadError;
use actix_web::http::header::{HeaderName, HeaderValue, CONTENT_TYPE};
use actix_web::web::Bytes;
use actix_web::{Error, HttpMessage};
use futures_core::Stream;
use serde_json::Value;
use std::future::{ready, Future, Ready};
use std::pin::Pin;
use std::rc::Rc;
use std::sync::Arc;
use std::task::{Context, Poll};

type OperationFn = dyn Fn(&ServiceRequest) -> String + Send + Sync;
type TenantFn = dyn Fn(&ServiceRequest) -> Option<String> + Send + Sync;

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

/// Middleware factory wrapping every request in the backend trace adapter.
#[derive(Clone)]
pub struct Reproit {
    config: Arc<MiddlewareConfig>,
}

impl Reproit {
    pub fn new(config: MiddlewareConfig) -> Self {
        Self {
            config: Arc::new(config),
        }
    }
}

impl<S, B> Transform<S, ServiceRequest> for Reproit
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error> + 'static,
    B: MessageBody + 'static,
{
    type Response = ServiceResponse<BoxBody>;
    type Error = Error;
    type Transform = ReproitMiddleware<S>;
    type InitError = ();
    type Future = Ready<Result<Self::Transform, Self::InitError>>;

    fn new_transform(&self, service: S) -> Self::Future {
        ready(Ok(ReproitMiddleware {
            service: Rc::new(service),
            config: self.config.clone(),
        }))
    }
}

/// The wrapped service; not constructed directly.
pub struct ReproitMiddleware<S> {
    service: Rc<S>,
    config: Arc<MiddlewareConfig>,
}

impl<S, B> Service<ServiceRequest> for ReproitMiddleware<S>
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error> + 'static,
    B: MessageBody + 'static,
{
    type Response = ServiceResponse<BoxBody>;
    type Error = Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>>>>;

    fn poll_ready(&self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.service.poll_ready(cx)
    }

    fn call(&self, mut request: ServiceRequest) -> Self::Future {
        let service = self.service.clone();
        let config = self.config.clone();
        Box::pin(async move {
            let pending = begin(&mut request, &config).await;
            let response = service.call(request).await?;
            match pending {
                None => Ok(response.map_into_boxed_body()),
                Some(pending) => Ok(finish(response, pending, &config).await),
            }
        })
    }
}

/// Begin the trace (fail closed: `None` leaves the request untraced),
/// re-buffering a bounded JSON payload so extractors still see it.
async fn begin(request: &mut ServiceRequest, config: &MiddlewareConfig) -> Option<PendingTrace> {
    let get_header = |name: &str| {
        request
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
    };
    let (context, scan) = resolve_context(get_header, config.capture.as_ref())?;
    let content_type = header_str(request.headers().get(CONTENT_TYPE));
    let content_length: Option<usize> = request
        .headers()
        .get("content-length")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse().ok());
    let body_json = match content_length {
        Some(length) if length > 0 && length <= MAX_BODY_BYTES => {
            let bytes = read_payload(request.take_payload(), length).await;
            let decoded = bytes
                .as_ref()
                .and_then(|bytes| decode_json(bytes, &content_type));
            request.set_payload(replay_payload(bytes.unwrap_or_default()));
            decoded
        }
        _ => None,
    };
    let operation = match &config.operation {
        Some(operation) => operation(request),
        None => format!("{} {}", request.method(), request.path()),
    };
    let tenant = config.tenant.as_ref().and_then(|tenant| tenant(request));
    let input = HttpInput {
        body: body_json,
        query: multi_map(query_pairs(request.query_string())),
        headers: multi_map(request.headers().iter().filter_map(|(name, value)| {
            Some((name.as_str().to_string(), value.to_str().ok()?.to_string()))
        })),
        ..HttpInput::default()
    };
    let trace = BackendTrace::begin(
        context,
        operation,
        None,
        tenant,
        None,
        input.into_value(),
        Vec::new(),
    )
    .ok()?;
    let recorder = Recorder::new(trace);
    request.extensions_mut().insert(recorder.clone());
    Some(PendingTrace { recorder, scan })
}

/// Finish the trace once the handler has produced the response, attach the
/// scan header or hand the trace to the capture sampler, and ship.
async fn finish<B: MessageBody + 'static>(
    response: ServiceResponse<B>,
    pending: PendingTrace,
    config: &MiddlewareConfig,
) -> ServiceResponse<BoxBody> {
    let Some(mut trace) = pending.recorder.take() else {
        return response.map_into_boxed_body();
    };
    let content_type = header_str(response.headers().get(CONTENT_TYPE));
    let (mut response, output) = match response.response().body().size() {
        BodySize::Sized(size) if size as usize <= MAX_BODY_BYTES => {
            let (request, head) = response.into_parts();
            let (head, body) = head.into_parts();
            match actix_web::body::to_bytes(body).await {
                Ok(bytes) => {
                    let decoded = decode_json(&bytes, &content_type);
                    let rebuilt = head.set_body(BoxBody::new(bytes));
                    (ServiceResponse::new(request, rebuilt), decoded)
                }
                // The body failed mid-read; nothing left to restore.
                Err(_) => {
                    let rebuilt = head.set_body(BoxBody::new(Bytes::new()));
                    (ServiceResponse::new(request, rebuilt), None)
                }
            }
        }
        _ => (response.map_into_boxed_body(), None),
    };
    let status = response.status().as_u16();
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
                response
                    .headers_mut()
                    .insert(HeaderName::from_static("x-reproit-events"), header);
            }
        } else if let Some(capture) = &config.capture {
            capture.record(&trace);
        }
    }
    response
}

fn header_str(value: Option<&HeaderValue>) -> String {
    value
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_string()
}

/// Read up to `expected` bytes (bounded by the content-length that was
/// already checked against the cap). `None` on payload errors.
async fn read_payload(mut payload: Payload, expected: usize) -> Option<Bytes> {
    let mut collected = Vec::with_capacity(expected.min(MAX_BODY_BYTES));
    loop {
        let chunk = std::future::poll_fn(|cx| Pin::new(&mut payload).poll_next(cx)).await;
        match chunk {
            None => return Some(Bytes::from(collected)),
            Some(Ok(bytes)) => {
                collected.extend_from_slice(&bytes);
                if collected.len() > MAX_BODY_BYTES {
                    return None;
                }
            }
            Some(Err(_)) => return None,
        }
    }
}

/// One-shot payload stream handing the buffered bytes back to extractors.
fn replay_payload(bytes: Bytes) -> Payload {
    struct Replay(Option<Bytes>);
    impl Stream for Replay {
        type Item = Result<Bytes, PayloadError>;
        fn poll_next(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            Poll::Ready(self.0.take().map(Ok))
        }
    }
    Payload::Stream {
        payload: Box::pin(Replay(if bytes.is_empty() { None } else { Some(bytes) })),
    }
}
