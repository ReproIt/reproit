//! The protobuf descriptor pool that gRPC replay resolves methods from.
//!
//! A run installs one pool before it invokes a gRPC method. The pool comes from
//! the `.proto` file the schema recorded (`pool_from_proto`), or from a real
//! `google.protobuf.FileDescriptorSet` in JSON form (`pool_from_protojson`). The
//! lossy in-memory descriptor that `schema_document.rs` builds for oracle
//! evaluation drops file names, enums, and oneofs, so it cannot rebuild a pool.
//! On that input `pool_from_protojson` returns a clear error and never panics.

use anyhow::{bail, Context, Result};
use prost_reflect::prost::Message;
use prost_reflect::{DescriptorPool, DeserializeOptions, DynamicMessage};
use serde_json::Value;
use std::path::Path;
use std::sync::OnceLock;

/// The run-global descriptor pool. One CLI process scans one service, so the
/// pool is process scope, and mirrors the `IDENTITY_POOL` pattern in
/// `transport.rs`. Installed once at the entry point where the schema is known.
static POOL: OnceLock<DescriptorPool> = OnceLock::new();

/// Install the pool for this run. The first install wins, so a second call with
/// an equal pool is a no-op (the test process installs the same pool per case).
pub(super) fn install(pool: DescriptorPool) {
    let _ = POOL.set(pool);
}

/// The installed pool, or `None` when no gRPC target was prepared.
pub(super) fn get() -> Option<DescriptorPool> {
    POOL.get().cloned()
}

/// Build a pool by compiling a `.proto` file with its parent as the import path.
/// This is the primary path: the schema records the `.proto` source, and replay
/// reads it back. Reuses the `protox::compile` call shape from
/// `schema_document.rs` so both read the same descriptors.
pub(super) fn pool_from_proto(path: &Path) -> Result<DescriptorPool> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let set = protox::compile([path], [parent])
        .with_context(|| format!("compiling protobuf schema {}", path.display()))?;
    DescriptorPool::from_file_descriptor_set(set)
        .with_context(|| format!("building descriptor pool from {}", path.display()))
}

/// The error returned for the lossy CLI descriptor. It names the fix (point the
/// scan at the `.proto` file) so the failure is actionable, not a panic.
const LOSSY_DESCRIPTOR: &str =
    "this gRPC descriptor JSON is the in-memory oracle form, which drops file names, \
     enums, and oneofs, so it cannot rebuild a message pool for replay. Point the scan \
     at the .proto source file instead.";

/// Build a pool from a real `google.protobuf.FileDescriptorSet` in JSON form.
/// The lossy oracle descriptor drops the file `name`, so a missing name is the
/// reject test: a real descriptor set names every file.
pub(super) fn pool_from_protojson(descriptor: &Value) -> Result<DescriptorPool> {
    let files = descriptor
        .get("file")
        .and_then(Value::as_array)
        .filter(|files| !files.is_empty())
        .context(LOSSY_DESCRIPTOR)?;
    for file in files {
        let named = file
            .get("name")
            .and_then(Value::as_str)
            .is_some_and(|name| !name.is_empty());
        if !named {
            bail!(LOSSY_DESCRIPTOR);
        }
    }
    // Bootstrap: the global pool describes google.protobuf.FileDescriptorSet, so
    // deserialize the JSON into a dynamic message of that type, re-encode it to
    // wire bytes, then decode those bytes into the target pool.
    let global = DescriptorPool::global();
    let set_descriptor = global
        .get_message_by_name("google.protobuf.FileDescriptorSet")
        .context("global pool is missing google.protobuf.FileDescriptorSet")?;
    let options = DeserializeOptions::new().deny_unknown_fields(false);
    let dynamic = DynamicMessage::deserialize_with_options(set_descriptor, descriptor, &options)
        .context("decoding gRPC descriptor JSON as a FileDescriptorSet")?;
    DescriptorPool::decode(dynamic.encode_to_vec().as_slice())
        .context("building descriptor pool from the gRPC descriptor JSON")
}

#[cfg(test)]
mod tests {
    use super::{pool_from_proto, pool_from_protojson};
    use prost_reflect::prost::Message;
    use prost_reflect::{DescriptorPool, DynamicMessage, SerializeOptions};
    use serde_json::{json, Value};
    use std::io::Write;

    const GREETER: &str = r#"
        syntax = "proto3";
        package helloworld;
        message HelloRequest { string name = 1; int64 count = 2; }
        message HelloReply { string message = 1; }
        service Greeter {
          rpc SayHello (HelloRequest) returns (HelloReply);
        }
    "#;

    fn write_proto(name: &str, body: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("reproit-grpc-desc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::File::create(&path)
            .unwrap()
            .write_all(body.as_bytes())
            .unwrap();
        path
    }

    #[test]
    fn proto_file_builds_a_pool_with_the_service() {
        let path = write_proto("greeter.proto", GREETER);
        let pool = pool_from_proto(&path).expect("pool from proto");
        let service = pool
            .get_service_by_name("helloworld.Greeter")
            .expect("Greeter service");
        assert!(service.methods().any(|method| method.name() == "SayHello"));
    }

    /// A real FileDescriptorSet in JSON rebuilds a pool. The JSON is produced by
    /// serializing the compiled descriptor set back through prost-reflect, so it
    /// is the genuine protojson form (every file named), not the lossy one.
    #[test]
    fn real_protojson_descriptor_set_builds_a_pool() {
        let path = write_proto("greeter_json.proto", GREETER);
        let set = protox::compile([path.as_path()], [path.parent().unwrap()]).unwrap();
        let global = DescriptorPool::global();
        let set_descriptor = global
            .get_message_by_name("google.protobuf.FileDescriptorSet")
            .unwrap();
        let dynamic =
            DynamicMessage::decode(set_descriptor, set.encode_to_vec().as_slice()).unwrap();
        let mut buffer = Vec::new();
        let mut serializer = serde_json::Serializer::new(&mut buffer);
        dynamic
            .serialize_with_options(&mut serializer, &SerializeOptions::default())
            .unwrap();
        let value: Value = serde_json::from_slice(&buffer).unwrap();
        let pool = pool_from_protojson(&value).expect("pool from real protojson");
        assert!(pool.get_service_by_name("helloworld.Greeter").is_some());
    }

    /// The default SerializeOptions match grpcurl's protojson output: a 64-bit
    /// integer is a string, and a field left at its default is omitted. Replay
    /// compares response JSON, so this fidelity has to hold.
    #[test]
    fn response_json_stringifies_int64_and_omits_defaults() {
        use prost_reflect::Value as ProtoValue;
        let path = write_proto("fidelity.proto", GREETER);
        let pool = pool_from_proto(&path).unwrap();
        let descriptor = pool.get_message_by_name("helloworld.HelloRequest").unwrap();
        let mut message = DynamicMessage::new(descriptor);
        message.set_field_by_name("count", ProtoValue::I64(9_007_199_254_740_993));
        // `name` is left at its default (empty string).
        let mut buffer = Vec::new();
        let mut serializer = serde_json::Serializer::new(&mut buffer);
        message
            .serialize_with_options(&mut serializer, &SerializeOptions::default())
            .unwrap();
        let value: Value = serde_json::from_slice(&buffer).unwrap();
        assert_eq!(
            value.get("count").and_then(Value::as_str),
            Some("9007199254740993")
        );
        assert!(
            value.get("name").is_none(),
            "default field is omitted: {value}"
        );
    }

    #[test]
    fn lossy_cli_descriptor_errors_without_panicking() {
        // The shape schema_document.rs builds for oracle evaluation: files carry
        // no name, so a pool cannot be rebuilt from it.
        let lossy = json!({
            "file": [{
                "package": "dogfood.v1",
                "messageType": [{ "name": "GetRequest", "field": [] }],
                "service": [{
                    "name": "Counters",
                    "method": [{
                        "name": "Get",
                        "inputType": ".dogfood.v1.GetRequest",
                        "outputType": ".dogfood.v1.GetRequest",
                    }],
                }],
            }],
        });
        let error = pool_from_protojson(&lossy).expect_err("lossy descriptor is rejected");
        assert!(
            error.to_string().contains("cannot rebuild a message pool"),
            "error names the lossy cause: {error}"
        );
    }
}
