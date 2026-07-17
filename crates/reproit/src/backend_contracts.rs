//! Supported backend-contract surface for validators and development tools.

pub use crate::model::backend::{
    evaluate, import_service_schema, parse_events, BackendConfig, BackendEvent, BackendViolation,
    OperationContract,
};
