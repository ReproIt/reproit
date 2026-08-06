//! In-process gRPC replay. Reads a request artifact, resolves its method from
//! the installed descriptor pool, and drives the HTTP/2 call with `tonic`. The
//! messages are dynamic: `prost-reflect` encodes the JSON request and decodes
//! the response against the method's input and output descriptors, so no
//! generated stubs and no downloaded `grpcurl` binary take part.

use super::RequestArtifact;
use anyhow::{anyhow, bail, Context, Result};
use http::uri::PathAndQuery;
use prost_reflect::prost::Message as _;
use prost_reflect::{
    DeserializeOptions, DynamicMessage, MessageDescriptor, MethodDescriptor, SerializeOptions,
};
use serde_json::Value;
use std::path::Path;
use std::time::Duration;
use tonic::client::Grpc;
use tonic::codec::{Codec, DecodeBuf, Decoder, EncodeBuf, Encoder};
use tonic::metadata::MetadataMap;
use tonic::transport::{Channel, ClientTlsConfig, Endpoint};
use tonic::{Request, Status};

mod descriptor;

/// The per-call timeout, matching the HTTP client build in `mod.rs`.
const CALL_TIMEOUT: Duration = Duration::from_secs(15);
/// Cap the number of messages a server stream may return, so a stream that
/// never ends cannot stall or exhaust the run.
const MAX_STREAM_MESSAGES: usize = 4_096;

/// Prepare the descriptor pool for this run. `proto` is the `.proto` file the
/// schema recorded; `document` is the parsed schema, used only when no `.proto`
/// path is known. Call once, before any gRPC invocation, and only when the run
/// has a gRPC target.
pub(super) fn prepare_pool(proto: Option<&Path>, document: Option<&Value>) -> Result<()> {
    let pool = match (proto, document) {
        (Some(path), _) => descriptor::pool_from_proto(path)?,
        (None, Some(document)) => descriptor::pool_from_protojson(document)?,
        (None, None) => bail!(
            "gRPC replay has no .proto schema source. Re-record the finding from the \
             .proto file so the message types can be rebuilt."
        ),
    };
    descriptor::install(pool);
    Ok(())
}

/// Invoke one gRPC operation and return its response as JSON. Signature matches
/// the former `invoke_grpc`: a unary call returns the single message, a
/// server-streaming call returns a JSON array of messages.
pub(super) async fn invoke(artifact: &RequestArtifact) -> Result<Value> {
    let pool = descriptor::get().context(
        "gRPC descriptor pool is not installed. This is a defect: the run must prepare \
         the pool before it invokes a gRPC method.",
    )?;
    let (service_name, method_name) = artifact.operation.split_once('/').with_context(|| {
        format!(
            "gRPC operation {:?} is not service/method",
            artifact.operation
        )
    })?;
    let service = pool
        .get_service_by_name(service_name)
        .with_context(|| format!("gRPC service {service_name} is not in the schema"))?;
    let method = service
        .methods()
        .find(|method| method.name() == method_name)
        .with_context(|| format!("gRPC method {method_name} is not in {service_name}"))?;

    let channel = connect(&artifact.url).await?;
    let metadata = MetadataMap::from_headers(super::transport::extra_headers()?);
    let mut client = Grpc::new(channel);
    client
        .ready()
        .await
        .map_err(|error| anyhow!("gRPC channel to {} is not ready: {error}", artifact.url))?;
    let path = PathAndQuery::try_from(format!("/{service_name}/{method_name}"))
        .with_context(|| format!("gRPC path /{service_name}/{method_name} is invalid"))?;
    dispatch(&mut client, artifact, &method, path, metadata).await
}

/// Connect a channel to the target. An `https` URL uses TLS with the webpki root
/// store; any other scheme connects in cleartext HTTP/2.
async fn connect(url: &str) -> Result<Channel> {
    let mut endpoint = Endpoint::from_shared(url.to_string())
        .with_context(|| format!("gRPC target {url} is not a valid URL"))?
        .timeout(CALL_TIMEOUT);
    if url.trim_start().starts_with("https://") {
        endpoint = endpoint
            .tls_config(ClientTlsConfig::new().with_enabled_roots())
            .context("gRPC TLS setup failed")?;
    }
    endpoint
        .connect()
        .await
        .with_context(|| format!("connecting to gRPC target {url}"))
}

/// Route the call by its streaming mode and collect the response as JSON.
async fn dispatch(
    client: &mut Grpc<Channel>,
    artifact: &RequestArtifact,
    method: &MethodDescriptor,
    path: PathAndQuery,
    metadata: MetadataMap,
) -> Result<Value> {
    let input = method.input();
    let codec = DynamicCodec {
        output: method.output(),
    };
    let body = artifact.body.as_ref().unwrap_or(&Value::Null);
    match (artifact.client_streaming, artifact.server_streaming) {
        (false, false) => {
            let request = request_of(single_message(&input, body)?, metadata);
            let response = client
                .unary(request, path, codec)
                .await
                .map_err(|status| call_error(&artifact.operation, &status))?;
            message_to_value(response.get_ref())
        }
        (true, false) => {
            let messages = stream_messages(&input, body)?;
            let request = request_of(tokio_stream(messages), metadata);
            let response = client
                .client_streaming(request, path, codec)
                .await
                .map_err(|status| call_error(&artifact.operation, &status))?;
            message_to_value(response.get_ref())
        }
        (false, true) => {
            let request = request_of(single_message(&input, body)?, metadata);
            let response = client
                .server_streaming(request, path, codec)
                .await
                .map_err(|status| call_error(&artifact.operation, &status))?;
            collect_stream(&artifact.operation, response.into_inner()).await
        }
        (true, true) => {
            let messages = stream_messages(&input, body)?;
            let request = request_of(tokio_stream(messages), metadata);
            let response = client
                .streaming(request, path, codec)
                .await
                .map_err(|status| call_error(&artifact.operation, &status))?;
            collect_stream(&artifact.operation, response.into_inner()).await
        }
    }
}

/// Build a request with the run metadata attached.
fn request_of<T>(message: T, metadata: MetadataMap) -> Request<T> {
    let mut request = Request::new(message);
    *request.metadata_mut() = metadata;
    request
}

/// Wrap a message vector as a `'static` stream for a client-streaming call.
fn tokio_stream(messages: Vec<DynamicMessage>) -> impl futures_util::Stream<Item = DynamicMessage> {
    futures_util::stream::iter(messages)
}

/// Encode one request message from JSON. A null body encodes the default
/// (empty) message, so a no-argument method needs no body.
fn single_message(descriptor: &MessageDescriptor, body: &Value) -> Result<DynamicMessage> {
    if body.is_null() {
        return Ok(DynamicMessage::new(descriptor.clone()));
    }
    decode_json(descriptor, body)
}

/// Encode a client-streaming body: a JSON array, one message per element.
fn stream_messages(descriptor: &MessageDescriptor, body: &Value) -> Result<Vec<DynamicMessage>> {
    let items = body
        .as_array()
        .context("gRPC client-streaming body must be a JSON array of messages")?;
    items
        .iter()
        .map(|item| decode_json(descriptor, item))
        .collect()
}

/// Decode one JSON value into a dynamic message. Unknown fields are accepted so
/// both the proto field name and the camelCase JSON name work.
fn decode_json(descriptor: &MessageDescriptor, value: &Value) -> Result<DynamicMessage> {
    let options = DeserializeOptions::new().deny_unknown_fields(false);
    DynamicMessage::deserialize_with_options(descriptor.clone(), value, &options)
        .context("encoding a gRPC request message from JSON")
}

/// Serialize a response message to JSON. The default options match grpcurl's
/// protojson: 64-bit integers as strings, default fields omitted, camelCase.
fn message_to_value(message: &DynamicMessage) -> Result<Value> {
    let mut buffer = Vec::new();
    let mut serializer = serde_json::Serializer::new(&mut buffer);
    message
        .serialize_with_options(&mut serializer, &SerializeOptions::default())
        .context("serializing a gRPC response message to JSON")?;
    serde_json::from_slice(&buffer).context("gRPC response JSON was invalid")
}

/// Collect a bounded server stream into a JSON array. The message count and the
/// total JSON size are both capped, so a hostile stream cannot stall the run.
async fn collect_stream(
    operation: &str,
    mut stream: tonic::Streaming<DynamicMessage>,
) -> Result<Value> {
    let mut messages = Vec::new();
    let mut bytes = 0usize;
    while let Some(message) = stream
        .message()
        .await
        .map_err(|status| call_error(operation, &status))?
    {
        let value = message_to_value(&message)?;
        bytes = bytes.saturating_add(value.to_string().len());
        if bytes > super::MAX_RESPONSE_BYTES {
            bail!(
                "gRPC stream exceeded the {} byte evidence limit",
                super::MAX_RESPONSE_BYTES
            );
        }
        messages.push(value);
        if messages.len() > MAX_STREAM_MESSAGES {
            bail!("gRPC stream exceeded the {MAX_STREAM_MESSAGES} message limit");
        }
    }
    Ok(Value::Array(messages))
}

/// Map a non-OK gRPC status to the same failure text the shell-out used, so the
/// run report reads the same as before the in-process cutover.
fn call_error(operation: &str, status: &Status) -> anyhow::Error {
    anyhow!(
        "gRPC operation {} failed: {:?}: {}",
        operation,
        status.code(),
        status.message()
    )
}

/// A tonic codec over dynamic messages: it encodes and decodes by descriptor,
/// so one code path serves every method in the schema.
struct DynamicCodec {
    output: MessageDescriptor,
}

impl Codec for DynamicCodec {
    type Encode = DynamicMessage;
    type Decode = DynamicMessage;
    type Encoder = DynamicEncoder;
    type Decoder = DynamicDecoder;

    fn encoder(&mut self) -> Self::Encoder {
        DynamicEncoder
    }

    fn decoder(&mut self) -> Self::Decoder {
        DynamicDecoder {
            output: self.output.clone(),
        }
    }
}

struct DynamicEncoder;

impl Encoder for DynamicEncoder {
    type Item = DynamicMessage;
    type Error = Status;

    fn encode(&mut self, item: Self::Item, dst: &mut EncodeBuf<'_>) -> Result<(), Status> {
        item.encode(dst)
            .map_err(|error| Status::internal(format!("encoding a gRPC message: {error}")))
    }
}

struct DynamicDecoder {
    output: MessageDescriptor,
}

impl Decoder for DynamicDecoder {
    type Item = DynamicMessage;
    type Error = Status;

    fn decode(&mut self, src: &mut DecodeBuf<'_>) -> Result<Option<Self::Item>, Status> {
        DynamicMessage::decode(self.output.clone(), src)
            .map(Some)
            .map_err(|error| Status::internal(format!("decoding a gRPC message: {error}")))
    }
}
