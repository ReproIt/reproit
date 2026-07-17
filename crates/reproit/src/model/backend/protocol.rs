use super::{hash, ValueDomain};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// Complete HTTP evidence for standards checks. Raw bytes are required because
/// decoded JSON or text cannot prove byte-range correctness.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HttpExchangeEvidence {
    pub request_method: String,
    #[serde(default)]
    pub request_headers: BTreeMap<String, String>,
    #[serde(default)]
    pub request_body: Vec<u8>,
    pub response_status: u16,
    #[serde(default)]
    pub response_headers: BTreeMap<String, String>,
    #[serde(default)]
    pub response_body: Vec<u8>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ProtocolEvidence {
    HttpByteRange {
        exchange: HttpExchangeEvidence,
        /// Exact bytes from an authored fixture or a validator-pinned immutable
        /// representation. A separately fetched dynamic body is not authority.
        authoritative_full_representation: Vec<u8>,
    },
    HttpRedirectTransition {
        redirect: HttpExchangeEvidence,
        next: HttpExchangeEvidence,
    },
    WebSocket {
        contract: WebSocketContract,
        evidence: WebSocketEvidence,
    },
}

impl ProtocolEvidence {
    pub(super) fn evaluate(&self) -> Vec<ProtocolViolation> {
        match self {
            Self::HttpByteRange {
                exchange,
                authoritative_full_representation,
            } => validate_http_byte_range(exchange, authoritative_full_representation)
                .into_iter()
                .collect(),
            Self::HttpRedirectTransition { redirect, next } => {
                validate_http_redirect_transition(redirect, next)
                    .into_iter()
                    .collect()
            }
            Self::WebSocket { contract, evidence } => {
                validate_websocket_contract(contract, evidence)
            }
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProtocolViolation {
    pub oracle: String,
    pub reason: String,
    pub fingerprint: String,
}

/// Check one single-range response against the exact full representation.
/// Multi-range responses, content encodings, weak validators, and incomplete
/// evidence abstain.
pub fn validate_http_byte_range(
    exchange: &HttpExchangeEvidence,
    full_representation: &[u8],
) -> Option<ProtocolViolation> {
    if !exchange.request_method.eq_ignore_ascii_case("GET")
        || exchange.response_status != 206
        || header(&exchange.request_headers, "if-range").is_some()
        || header(&exchange.response_headers, "content-encoding")
            .is_some_and(|value| !value.eq_ignore_ascii_case("identity"))
    {
        return None;
    }
    let requested = parse_single_byte_range(
        header(&exchange.request_headers, "range")?,
        full_representation.len() as u64,
    )?;
    let (start, end, complete) =
        parse_content_range(header(&exchange.response_headers, "content-range")?)?;
    let expected_end = requested.1;
    let expected = full_representation.get(requested.0 as usize..=expected_end as usize)?;
    let wrong = (start, end, complete)
        != (requested.0, expected_end, full_representation.len() as u64)
        || exchange.response_body != expected
        || header(&exchange.response_headers, "content-length")
            .and_then(|value| value.parse::<usize>().ok())
            .is_some_and(|length| length != expected.len());
    wrong.then(|| {
        protocol_violation(
            "http-byte-range",
            format!(
                "Range bytes={}-{} did not return the exact {}-byte slice of the {}-byte \
                 representation",
                requested.0,
                requested.1,
                expected.len(),
                full_representation.len()
            ),
        )
    })
}

fn parse_single_byte_range(value: &str, length: u64) -> Option<(u64, u64)> {
    let raw = value.trim().strip_prefix("bytes=")?;
    if raw.contains(',') || length == 0 {
        return None;
    }
    let (start, end) = raw.split_once('-')?;
    if start.is_empty() {
        let suffix = end.parse::<u64>().ok()?;
        if suffix == 0 {
            return None;
        }
        return Some((length.saturating_sub(suffix.min(length)), length - 1));
    }
    let start = start.parse::<u64>().ok()?;
    if start >= length {
        return None;
    }
    let end = if end.is_empty() {
        length - 1
    } else {
        end.parse::<u64>().ok()?.min(length - 1)
    };
    (start <= end).then_some((start, end))
}

fn parse_content_range(value: &str) -> Option<(u64, u64, u64)> {
    let raw = value.trim().strip_prefix("bytes ")?;
    let (range, complete) = raw.split_once('/')?;
    let (start, end) = range.split_once('-')?;
    Some((
        start.parse().ok()?,
        end.parse().ok()?,
        complete.parse().ok()?,
    ))
}

/// Validate a redirect hop against RFC method rewriting rules. The caller
/// supplies the observed next request, so absence of a follow-up hop abstains.
pub fn validate_http_redirect_transition(
    redirect: &HttpExchangeEvidence,
    next: &HttpExchangeEvidence,
) -> Option<ProtocolViolation> {
    let status = redirect.response_status;
    if !matches!(status, 301 | 302 | 303 | 307 | 308)
        || header(&redirect.response_headers, "location").is_none()
    {
        return None;
    }
    let original = redirect.request_method.to_ascii_uppercase();
    let (method_wrong, body_wrong, expectation) = if status == 303 {
        let expected = if original == "HEAD" { "HEAD" } else { "GET" };
        (
            !next.request_method.eq_ignore_ascii_case(expected),
            !next.request_body.is_empty(),
            format!("use {expected} with no carried request body"),
        )
    } else if matches!(status, 301 | 302) && original == "POST" {
        // RFC 9110 permits user agents to rewrite POST to GET for historical
        // compatibility. Both that transition and exact POST preservation are
        // conforming, so neither is flagged.
        let valid_get =
            next.request_method.eq_ignore_ascii_case("GET") && next.request_body.is_empty();
        let valid_preserved = next.request_method.eq_ignore_ascii_case("POST")
            && next.request_body == redirect.request_body;
        (
            !valid_get && !valid_preserved,
            false,
            "either rewrite POST to bodyless GET or preserve POST and its body".into(),
        )
    } else {
        (
            !next.request_method.eq_ignore_ascii_case(&original),
            next.request_body != redirect.request_body,
            format!("preserve {original} and its request body"),
        )
    };
    (method_wrong || body_wrong).then(|| {
        protocol_violation(
            "http-redirect-transition",
            format!("HTTP {status} from {original} required the next request to {expectation}"),
        )
    })
}

fn header<'a>(headers: &'a BTreeMap<String, String>, name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

fn protocol_violation(oracle: &str, reason: String) -> ProtocolViolation {
    let fingerprint = hash(format!("{oracle}:{reason}").as_bytes())[..20].to_string();
    ProtocolViolation {
        oracle: oracle.into(),
        reason,
        fingerprint,
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebSocketContract {
    pub route: String,
    #[serde(default)]
    pub allowed_principals: BTreeSet<String>,
    #[serde(default)]
    pub denied_principals: BTreeSet<String>,
    #[serde(default)]
    pub allowed_client_messages: Vec<ValueDomain>,
    #[serde(default)]
    pub allowed_server_messages: Vec<ValueDomain>,
    #[serde(default)]
    pub denied_close_codes: BTreeSet<u16>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebSocketEvidence {
    pub route: String,
    pub principal: String,
    pub accepted: bool,
    #[serde(default)]
    pub close_code: Option<u16>,
    #[serde(default)]
    pub client_messages: Vec<Value>,
    #[serde(default)]
    pub server_messages: Vec<Value>,
}

/// Evaluate only an authored WebSocket contract. An undeclared principal,
/// route, message direction, or empty schema list abstains.
pub fn validate_websocket_contract(
    contract: &WebSocketContract,
    evidence: &WebSocketEvidence,
) -> Vec<ProtocolViolation> {
    if contract.route != evidence.route {
        return Vec::new();
    }
    let mut violations = Vec::new();
    if contract.denied_principals.contains(&evidence.principal) && evidence.accepted {
        violations.push(protocol_violation(
            "websocket-authorization",
            format!(
                "principal {:?} was explicitly denied but the WebSocket was accepted",
                evidence.principal
            ),
        ));
    } else if contract.allowed_principals.contains(&evidence.principal) && !evidence.accepted {
        violations.push(protocol_violation(
            "websocket-authorization",
            format!(
                "principal {:?} was explicitly allowed but the WebSocket was rejected",
                evidence.principal
            ),
        ));
    }
    if let Some(code) = evidence.close_code {
        if contract.denied_close_codes.contains(&code) {
            violations.push(protocol_violation(
                "websocket-close",
                format!("the authored contract forbids close code {code}"),
            ));
        }
    }
    validate_websocket_messages(
        "client",
        &contract.allowed_client_messages,
        &evidence.client_messages,
        &mut violations,
    );
    validate_websocket_messages(
        "server",
        &contract.allowed_server_messages,
        &evidence.server_messages,
        &mut violations,
    );
    violations
}

fn validate_websocket_messages(
    direction: &str,
    schemas: &[ValueDomain],
    messages: &[Value],
    violations: &mut Vec<ProtocolViolation>,
) {
    if schemas.is_empty() {
        return;
    }
    for (index, message) in messages.iter().enumerate() {
        if schemas
            .iter()
            .all(|schema| schema.mismatch(message, "$message").is_some())
        {
            violations.push(protocol_violation(
                "websocket-message",
                format!("{direction} message {index} contradicted every authored message schema"),
            ));
        }
    }
}
