//! Supported backend-contract surface for validators and development tools.

pub use crate::domain::backend::{
    evaluate, import_service_schema, parse_events, validate_http_conditional_cache,
    validate_http_response_media_type, validate_protocol_lifecycle, BackendConfig, BackendEvent,
    BackendProofContract, BackendViolation, CodecProjection, HttpExchangeEvidence,
    OperationContract, ProtocolEvidence, ProtocolLifecycleContract, ProtocolLifecycleEvent,
    ProtocolLifecycleEvidence, ProtocolLifecycleRule,
};
