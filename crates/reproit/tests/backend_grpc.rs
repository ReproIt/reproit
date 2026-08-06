//! In-process gRPC replay, end to end through the real `reproit` binary.
//!
//! A raw HTTP/2 server answers gRPC calls by hand: it reads the length-prefixed
//! request frames and replies with an encoded message plus `grpc-status`
//! trailers. `reproit fuzz` reaches it over the same in-process client that
//! replaced the downloaded `grpcurl` binary, so the whole path is hermetic: no
//! network, no Go, no external tool.

use bytes::Bytes;
use http::{HeaderMap, HeaderValue, Response};
use serde_json::Value;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A HelloReply message with `message = "hi"`, protobuf wire form: field 1
/// (string), length 2, bytes "hi".
const HELLO_REPLY: &[u8] = &[0x0A, 0x02, b'h', b'i'];

/// Wrap message bytes in one gRPC data frame: a compression flag byte, a
/// four-byte big-endian length, then the message.
fn grpc_frame(message: &[u8]) -> Bytes {
    let mut frame = Vec::with_capacity(5 + message.len());
    frame.push(0);
    frame.extend_from_slice(&(message.len() as u32).to_be_bytes());
    frame.extend_from_slice(message);
    Bytes::from(frame)
}

/// Start the raw gRPC server on an ephemeral port and return the port. The
/// server runs on its own thread with its own runtime, so a blocking
/// `reproit` subprocess can call it.
fn start_server() -> u16 {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("server runtime");
        runtime.block_on(async move {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind gRPC server");
            tx.send(listener.local_addr().unwrap().port()).unwrap();
            loop {
                let (socket, _) = listener.accept().await.expect("accept");
                tokio::spawn(async move {
                    let _ = serve_connection(socket).await;
                });
            }
        });
    });
    rx.recv().expect("server port")
}

/// Answer every gRPC stream on one connection. Each stream is handled in its own
/// task so the accept loop keeps driving connection I/O (flushing the replies).
async fn serve_connection(socket: tokio::net::TcpStream) -> Result<(), h2::Error> {
    let mut connection = h2::server::handshake(socket).await?;
    while let Some(request) = connection.accept().await {
        let (request, respond) = request?;
        tokio::spawn(async move {
            let _ = handle_stream(request, respond).await;
        });
    }
    Ok(())
}

/// Answer one gRPC call by method name.
async fn handle_stream(
    request: http::Request<h2::RecvStream>,
    mut respond: h2::server::SendResponse<Bytes>,
) -> Result<(), h2::Error> {
    let method = request
        .uri()
        .path()
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .to_string();
    let has_meta = request.headers().contains_key("x-reproit-test");
    // Drain the request body so client-streaming frames are read and flow
    // control is released before the reply.
    let mut body = request.into_body();
    while let Some(chunk) = body.data().await {
        let chunk = chunk?;
        let _ = body.flow_control().release_capacity(chunk.len());
    }
    match method.as_str() {
        "Fail" => {
            respond.send_response(grpc_headers(Some("13")), true)?;
        }
        "NeedMeta" if !has_meta => {
            respond.send_response(grpc_headers(Some("9")), true)?;
        }
        other => {
            let mut stream = respond.send_response(grpc_headers(None), false)?;
            let count = if other == "SayHellos" { 3 } else { 1 };
            for _ in 0..count {
                stream.send_data(grpc_frame(HELLO_REPLY), false)?;
            }
            let mut trailers = HeaderMap::new();
            trailers.insert("grpc-status", HeaderValue::from_static("0"));
            stream.send_trailers(trailers)?;
        }
    }
    Ok(())
}

/// Build gRPC response headers. `trailers_only` sets `grpc-status` in the header
/// frame itself, which is how a call reports a status with no message body.
fn grpc_headers(trailers_only: Option<&str>) -> Response<()> {
    let mut builder = Response::builder()
        .status(200)
        .header("content-type", "application/grpc");
    if let Some(status) = trailers_only {
        builder = builder.header("grpc-status", status);
    }
    builder.body(()).expect("gRPC response headers")
}

const MESSAGES: &str = "message HelloRequest { string name = 1; }\n\
                        message HelloReply { string message = 1; }\n";

/// Write a proto file that declares the given RPC lines, and return its path.
fn write_proto(dir: &Path, name: &str, rpcs: &str) -> PathBuf {
    let body = format!(
        "syntax = \"proto3\";\npackage helloworld;\n{MESSAGES}service Greeter {{\n{rpcs}}}\n"
    );
    let path = dir.join(name);
    std::fs::File::create(&path)
        .unwrap()
        .write_all(body.as_bytes())
        .unwrap();
    path
}

/// Run `reproit fuzz` against one proto and return the parsed JSON report.
fn fuzz(dir: &Path, proto: &Path, port: u16, header: Option<&str>) -> Value {
    let mut command = Command::new(env!("CARGO_BIN_EXE_reproit"));
    command
        .current_dir(dir)
        .env("REPROIT_BACKEND_URL", format!("http://127.0.0.1:{port}"))
        .args(["--json", "fuzz"])
        .arg(proto)
        .args(["--runs", "1", "--yes"]);
    if let Some(header) = header {
        command.env("REPROIT_EXTRA_HEADERS", header);
    }
    let Output { stdout, status, .. } = command.output().expect("run reproit fuzz");
    assert!(
        !stdout.is_empty(),
        "reproit produced no report (exit {:?})",
        status.code()
    );
    serde_json::from_slice(&stdout).expect("reproit report is JSON")
}

fn findings_len(report: &Value) -> usize {
    report["findings"].as_array().map_or(0, Vec::len)
}

#[test]
fn in_process_grpc_round_trips_unary_and_streams() {
    let port = start_server();
    let dir = std::env::temp_dir().join(format!("reproit-grpc-it-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();

    // Unary, server streaming, and client streaming in one clean fuzz.
    let clean = write_proto(
        &dir,
        "clean.proto",
        "  rpc SayHello (HelloRequest) returns (HelloReply);\n\
         \x20 rpc SayHellos (HelloRequest) returns (stream HelloReply);\n\
         \x20 rpc CollectHellos (stream HelloRequest) returns (HelloReply);\n",
    );
    let report = fuzz(&dir, &clean, port, None);
    assert_eq!(report["complete"], Value::Bool(true), "clean run: {report}");
    assert_eq!(
        findings_len(&report),
        0,
        "clean run has no findings: {report}"
    );
    assert!(
        report["exercised"].as_u64().unwrap_or(0) >= 3,
        "all three gRPC methods were exercised: {report}"
    );
    assert!(
        report["executionErrors"]
            .as_array()
            .is_none_or(Vec::is_empty),
        "clean run has no execution errors: {report}"
    );

    // A non-zero grpc-status surfaces as an execution error, not a pass.
    let failing = write_proto(
        &dir,
        "fail.proto",
        "  rpc Fail (HelloRequest) returns (HelloReply);\n",
    );
    let report = fuzz(&dir, &failing, port, None);
    let errors = report["executionErrors"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        errors.iter().any(|error| error["error"]
            .as_str()
            .is_some_and(|text| text.contains("failed"))),
        "non-zero grpc-status is reported as a failure: {report}"
    );

    // Metadata reaches the server: NeedMeta answers OK only when the header is
    // present, so a clean run proves REPROIT_EXTRA_HEADERS propagated.
    let meta = write_proto(
        &dir,
        "meta.proto",
        "  rpc NeedMeta (HelloRequest) returns (HelloReply);\n",
    );
    let with_header = fuzz(&dir, &meta, port, Some(r#"{"x-reproit-test":"present"}"#));
    assert_eq!(
        with_header["complete"],
        Value::Bool(true),
        "metadata run is clean when the header is set: {with_header}"
    );
    assert_eq!(findings_len(&with_header), 0);
    let without_header = fuzz(&dir, &meta, port, None);
    let errors = without_header["executionErrors"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        !errors.is_empty(),
        "without the header the metadata check fails: {without_header}"
    );

    std::fs::remove_dir_all(&dir).ok();
}
