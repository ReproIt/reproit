//! Minimal std-only stub ingest server for the middleware tests: accepts
//! one HTTP POST per connection, parses the JSON batch, and replies 200.

use serde_json::Value;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::mpsc::{channel, Receiver};

pub struct StubIngest {
    pub url: String,
    pub received: Receiver<(String, Value)>,
}

pub fn start_stub_ingest() -> StubIngest {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind stub ingest");
    let url = format!("http://{}/v1/events", listener.local_addr().unwrap());
    let (sender, received) = channel();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { return };
            let mut reader = BufReader::new(stream);
            let mut authorization = String::new();
            let mut content_length = 0usize;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).is_err() || line.trim_end().is_empty() {
                    break;
                }
                let lower = line.to_ascii_lowercase();
                if let Some(value) = lower.strip_prefix("authorization:") {
                    authorization = value.trim().to_string();
                }
                if let Some(value) = lower.strip_prefix("content-length:") {
                    content_length = value.trim().parse().unwrap_or(0);
                }
            }
            let mut body = vec![0u8; content_length];
            if reader.read_exact(&mut body).is_err() {
                return;
            }
            let response = "HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\
                            content-type: application/json\r\n\r\n{}";
            let mut stream = reader.into_inner();
            let _ = stream.write_all(response.as_bytes());
            if let Ok(batch) = serde_json::from_slice(&body) {
                if sender.send((authorization, batch)).is_err() {
                    return;
                }
            }
        }
    });
    StubIngest { url, received }
}
