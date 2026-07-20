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
    pub request_target: String,
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
    /// Exact response bytes checked against an authored set of media types.
    /// Missing or malformed Content-Type evidence abstains.
    HttpResponseMediaType {
        exchange: HttpExchangeEvidence,
        allowed_media_types: BTreeSet<String>,
    },
    HttpConditionalCache {
        initial: HttpExchangeEvidence,
        conditional: HttpExchangeEvidence,
    },
    /// An application-authored lifecycle contract evaluated against one
    /// complete, stably identified scope trace.
    Lifecycle {
        contract: ProtocolLifecycleContract,
        evidence: ProtocolLifecycleEvidence,
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
            Self::HttpResponseMediaType {
                exchange,
                allowed_media_types,
            } => validate_http_response_media_type(exchange, allowed_media_types)
                .into_iter()
                .collect(),
            Self::HttpConditionalCache {
                initial,
                conditional,
            } => validate_http_conditional_cache(initial, conditional)
                .into_iter()
                .collect(),
            Self::Lifecycle { contract, evidence } => {
                validate_protocol_lifecycle(contract, evidence)
            }
            Self::WebSocket { contract, evidence } => {
                validate_websocket_contract(contract, evidence)
            }
        }
    }
}

/// Authored ordering and ownership rules for one kind of stable scope.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProtocolLifecycleContract {
    pub scope_kind: String,
    #[serde(default)]
    pub rules: Vec<ProtocolLifecycleRule>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ProtocolLifecycleRule {
    /// Every occurrence of `before` must precede every occurrence of `after`.
    Precedence { before: String, after: String },
    /// The named event must not occur after the boundary event.
    ForbidAfter { event: String, boundary: String },
    /// Bounds the number of occurrences in the complete scope trace.
    Cardinality {
        event: String,
        #[serde(default)]
        at_least: usize,
        #[serde(default)]
        at_most: Option<usize>,
    },
}

/// One event in a lifecycle trace. Sequence values establish the total order;
/// wall-clock timestamps are deliberately not accepted as ordering authority.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProtocolLifecycleEvent {
    pub sequence: u64,
    pub name: String,
    pub scope_id: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProtocolLifecycleEvidence {
    pub scope_kind: String,
    pub scope_id: String,
    /// True only when the producer observed the entire authored scope, from
    /// creation through termination, without dropped events.
    pub complete: bool,
    #[serde(default)]
    pub events: Vec<ProtocolLifecycleEvent>,
}

/// Evaluate lifecycle rules only when the evidence proves one complete,
/// unambiguous, totally ordered scope. Invalid rules abstain individually;
/// incomplete or ambiguous evidence abstains for the whole contract.
pub fn validate_protocol_lifecycle(
    contract: &ProtocolLifecycleContract,
    evidence: &ProtocolLifecycleEvidence,
) -> Vec<ProtocolViolation> {
    if !evidence.complete
        || contract.scope_kind.trim().is_empty()
        || evidence.scope_kind != contract.scope_kind
        || evidence.scope_id.trim().is_empty()
        || evidence.events.iter().any(|event| {
            event.name.trim().is_empty()
                || event.scope_id.trim().is_empty()
                || event.scope_id != evidence.scope_id
        })
    {
        return Vec::new();
    }
    let mut sequences = BTreeSet::new();
    if evidence
        .events
        .iter()
        .any(|event| !sequences.insert(event.sequence))
    {
        return Vec::new();
    }

    let positions = |name: &str| {
        evidence
            .events
            .iter()
            .filter(|event| event.name == name)
            .map(|event| event.sequence)
            .collect::<Vec<_>>()
    };
    let mut violations = Vec::new();
    for rule in &contract.rules {
        match rule {
            ProtocolLifecycleRule::Precedence { before, after }
                if valid_lifecycle_name(before)
                    && valid_lifecycle_name(after)
                    && before != after =>
            {
                let earlier = positions(before);
                let later = positions(after);
                if earlier
                    .iter()
                    .max()
                    .zip(later.iter().min())
                    .is_some_and(|(last_earlier, first_later)| last_earlier > first_later)
                {
                    violations.push(protocol_violation(
                        "lifecycle-precedence",
                        format!(
                            "authored lifecycle requires {before:?} to precede every \
                                 {after:?} event"
                        ),
                    ));
                }
            }
            ProtocolLifecycleRule::ForbidAfter { event, boundary }
                if valid_lifecycle_name(event)
                    && valid_lifecycle_name(boundary)
                    && event != boundary =>
            {
                let forbidden = positions(event);
                let boundaries = positions(boundary);
                if forbidden.iter().any(|position| {
                    boundaries
                        .iter()
                        .any(|boundary_position| position > boundary_position)
                }) {
                    violations.push(protocol_violation(
                        "lifecycle-forbid-after",
                        format!("authored lifecycle forbids {event:?} after {boundary:?}"),
                    ));
                }
            }
            ProtocolLifecycleRule::Cardinality {
                event,
                at_least,
                at_most,
            } if valid_lifecycle_name(event)
                && at_most.is_none_or(|maximum| *at_least <= maximum)
                && (*at_least > 0 || at_most.is_some()) =>
            {
                let count = positions(event).len();
                if count < *at_least || at_most.is_some_and(|maximum| count > maximum) {
                    let expected = at_most.map_or_else(
                        || format!("at least {at_least}"),
                        |maximum| {
                            if maximum == *at_least {
                                format!("exactly {maximum}")
                            } else {
                                format!("between {at_least} and {maximum}")
                            }
                        },
                    );
                    violations.push(protocol_violation(
                        "lifecycle-cardinality",
                        format!(
                            "authored lifecycle requires {event:?} {expected} time(s), but \
                             observed {count}"
                        ),
                    ));
                }
            }
            _ => {}
        }
    }
    violations
}

fn valid_lifecycle_name(name: &str) -> bool {
    !name.trim().is_empty() && name.len() <= 128
}

/// Validate a conditional GET against the exact representation and validator
/// from its initial GET. Different `Vary` request dimensions, compound
/// validators, and incomplete evidence abstain.
pub fn validate_http_conditional_cache(
    initial: &HttpExchangeEvidence,
    conditional: &HttpExchangeEvidence,
) -> Option<ProtocolViolation> {
    if !initial.request_method.eq_ignore_ascii_case("GET")
        || !conditional.request_method.eq_ignore_ascii_case("GET")
        || initial.request_target != conditional.request_target
        || initial.response_status != 200
        || !matches!(conditional.response_status, 200 | 304)
    {
        return None;
    }
    let initial_etag = header(&initial.response_headers, "etag")?.trim();
    let validator = header(&conditional.request_headers, "if-none-match")?.trim();
    let (initial_weak, initial_opaque) = parse_single_etag(initial_etag)?;
    let (_, validator_opaque) = parse_single_etag(validator)?;
    if validator_opaque != initial_opaque || !same_vary_dimensions(initial, conditional) {
        return None;
    }
    if conditional.response_status == 304 {
        if !conditional.response_body.is_empty() {
            return Some(protocol_violation(
                "http-conditional-cache",
                "HTTP 304 carried a response body".into(),
            ));
        }
        if let Some(returned) = header(&conditional.response_headers, "etag") {
            let (_, returned_opaque) = parse_single_etag(returned.trim())?;
            if returned_opaque != initial_opaque {
                return Some(protocol_violation(
                    "http-conditional-cache",
                    "HTTP 304 returned an ETag that contradicted the matched validator".into(),
                ));
            }
        }
        return None;
    }
    let returned_etag = header(&conditional.response_headers, "etag")?.trim();
    let (returned_weak, returned_opaque) = parse_single_etag(returned_etag)?;
    if !initial_weak
        && !returned_weak
        && returned_opaque == initial_opaque
        && header(&initial.response_headers, "content-encoding")
            == header(&conditional.response_headers, "content-encoding")
        && conditional.response_body != initial.response_body
    {
        return Some(protocol_violation(
            "http-conditional-cache",
            "the same strong ETag identified different exact response bytes".into(),
        ));
    }
    None
}

fn parse_single_etag(value: &str) -> Option<(bool, &str)> {
    let (weak, tag) = value
        .strip_prefix("W/")
        .map_or((false, value), |tag| (true, tag));
    if tag.len() < 2 || !tag.starts_with('"') || !tag.ends_with('"') {
        return None;
    }
    let opaque = &tag[1..tag.len() - 1];
    (!opaque.contains('"')).then_some((weak, opaque))
}

fn same_vary_dimensions(
    initial: &HttpExchangeEvidence,
    conditional: &HttpExchangeEvidence,
) -> bool {
    let Some(vary) = header(&initial.response_headers, "vary") else {
        return true;
    };
    let names = vary.split(',').map(str::trim).collect::<Vec<_>>();
    !names.is_empty()
        && names.iter().all(|name| {
            !name.is_empty()
                && *name != "*"
                && header(&initial.request_headers, name)
                    == header(&conditional.request_headers, name)
        })
}

/// Check an exact response body against an application-authored media type.
/// The validator deliberately supports only JSON types, whose byte grammar is
/// unambiguous here. Other types and bodyless responses abstain.
pub fn validate_http_response_media_type(
    exchange: &HttpExchangeEvidence,
    allowed_media_types: &BTreeSet<String>,
) -> Option<ProtocolViolation> {
    if allowed_media_types.is_empty()
        || exchange.request_method.eq_ignore_ascii_case("HEAD")
        || matches!(exchange.response_status, 100..=199 | 204 | 304)
        || exchange.response_body.is_empty()
    {
        return None;
    }
    let raw = header(&exchange.response_headers, "content-type")?;
    let media_type = raw.split(';').next()?.trim().to_ascii_lowercase();
    if !valid_media_type_token(&media_type) {
        return None;
    }
    let allowed = allowed_media_types
        .iter()
        .filter_map(|value| {
            let normalized = value.split(';').next()?.trim().to_ascii_lowercase();
            valid_media_type_token(&normalized).then_some(normalized)
        })
        .collect::<BTreeSet<_>>();
    if allowed.is_empty() {
        return None;
    }
    if !allowed.contains(&media_type) {
        return Some(protocol_violation(
            "http-response-media-type",
            format!(
                "response Content-Type {media_type} was outside the authored media types {}",
                allowed.into_iter().collect::<Vec<_>>().join(", ")
            ),
        ));
    }
    if (media_type == "application/json" || media_type.ends_with("+json"))
        && serde_json::from_slice::<Value>(&exchange.response_body).is_err()
    {
        return Some(protocol_violation(
            "http-response-media-type",
            format!("response declared {media_type} but its exact body was not valid JSON"),
        ));
    }
    None
}

fn valid_media_type_token(value: &str) -> bool {
    let Some((kind, subtype)) = value.split_once('/') else {
        return false;
    };
    !kind.is_empty()
        && !subtype.is_empty()
        && !subtype.contains('/')
        && kind
            .bytes()
            .chain(subtype.bytes())
            .all(|byte| byte.is_ascii_alphanumeric() || b"!#$&^_.+-".contains(&byte))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn exchange(status: u16, body: &[u8]) -> HttpExchangeEvidence {
        HttpExchangeEvidence {
            request_method: "GET".into(),
            request_target: "/resource".into(),
            request_headers: BTreeMap::new(),
            request_body: Vec::new(),
            response_status: status,
            response_headers: BTreeMap::new(),
            response_body: body.to_vec(),
        }
    }

    #[test]
    fn authored_json_media_type_checks_exact_body_and_abstains_without_authority() {
        let allowed = BTreeSet::from(["application/json".into()]);
        let mut response = exchange(200, br#"{"ok":true}"#);
        response.response_headers.insert(
            "Content-Type".into(),
            "application/json; charset=utf-8".into(),
        );
        assert!(validate_http_response_media_type(&response, &allowed).is_none());

        response.response_body = b"not-json".to_vec();
        let violation = validate_http_response_media_type(&response, &allowed).unwrap();
        assert_eq!(violation.oracle, "http-response-media-type");

        response.response_headers.clear();
        assert!(validate_http_response_media_type(&response, &allowed).is_none());
        assert!(validate_http_response_media_type(&response, &BTreeSet::new()).is_none());
    }

    #[test]
    fn conditional_cache_requires_matching_vary_and_proves_exact_contradictions() {
        let mut initial = exchange(200, b"first");
        initial
            .request_headers
            .insert("Accept-Language".into(), "en".into());
        initial
            .response_headers
            .insert("ETag".into(), "\"v1\"".into());
        initial
            .response_headers
            .insert("Vary".into(), "Accept-Language".into());

        let mut conditional = exchange(304, b"forbidden");
        conditional
            .request_headers
            .insert("If-None-Match".into(), "\"v1\"".into());
        conditional
            .request_headers
            .insert("accept-language".into(), "en".into());
        let violation = validate_http_conditional_cache(&initial, &conditional).unwrap();
        assert!(violation.reason.contains("304"));

        conditional.response_body.clear();
        conditional
            .response_headers
            .insert("ETag".into(), "\"v2\"".into());
        assert!(validate_http_conditional_cache(&initial, &conditional)
            .unwrap()
            .reason
            .contains("contradicted"));

        conditional.response_status = 200;
        conditional.response_body = b"second".to_vec();
        conditional
            .response_headers
            .insert("ETag".into(), "\"v1\"".into());
        assert!(validate_http_conditional_cache(&initial, &conditional)
            .unwrap()
            .reason
            .contains("strong ETag"));

        conditional
            .request_headers
            .insert("Accept-Language".into(), "fr".into());
        assert!(validate_http_conditional_cache(&initial, &conditional).is_none());
    }

    #[test]
    fn conditional_cache_weak_tags_do_not_claim_byte_identity() {
        let mut initial = exchange(200, b"first");
        initial
            .response_headers
            .insert("ETag".into(), "W/\"v1\"".into());
        let mut conditional = exchange(200, b"second");
        conditional
            .request_headers
            .insert("If-None-Match".into(), "\"v1\"".into());
        conditional
            .response_headers
            .insert("ETag".into(), "W/\"v1\"".into());
        assert!(validate_http_conditional_cache(&initial, &conditional).is_none());
    }

    fn lifecycle_contract() -> ProtocolLifecycleContract {
        ProtocolLifecycleContract {
            scope_kind: "request".into(),
            rules: vec![
                ProtocolLifecycleRule::Precedence {
                    before: "resource.acquire".into(),
                    after: "resource.use".into(),
                },
                ProtocolLifecycleRule::ForbidAfter {
                    event: "callback".into(),
                    boundary: "request.close".into(),
                },
                ProtocolLifecycleRule::Cardinality {
                    event: "resource.release".into(),
                    at_least: 1,
                    at_most: Some(1),
                },
            ],
        }
    }

    fn lifecycle_evidence(events: &[(u64, &str)]) -> ProtocolLifecycleEvidence {
        ProtocolLifecycleEvidence {
            scope_kind: "request".into(),
            scope_id: "request-42".into(),
            complete: true,
            events: events
                .iter()
                .map(|(sequence, name)| ProtocolLifecycleEvent {
                    sequence: *sequence,
                    name: (*name).into(),
                    scope_id: "request-42".into(),
                })
                .collect(),
        }
    }

    #[test]
    fn lifecycle_protocol_proves_order_after_close_and_ownership_violations() {
        let evidence = lifecycle_evidence(&[
            (0, "resource.use"),
            (1, "resource.acquire"),
            (2, "request.close"),
            (3, "callback"),
            (4, "resource.release"),
            (5, "resource.release"),
        ]);
        let proof = ProtocolEvidence::Lifecycle {
            contract: lifecycle_contract(),
            evidence,
        };
        let violations = proof.evaluate();
        assert_eq!(violations.len(), 3);
        assert_eq!(
            violations
                .iter()
                .map(|violation| violation.oracle.as_str())
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([
                "lifecycle-cardinality",
                "lifecycle-forbid-after",
                "lifecycle-precedence",
            ])
        );
    }

    #[test]
    fn lifecycle_protocol_accepts_a_complete_satisfied_scope() {
        let evidence = lifecycle_evidence(&[
            (0, "resource.acquire"),
            (1, "resource.use"),
            (2, "request.close"),
            (3, "resource.release"),
        ]);
        assert!(validate_protocol_lifecycle(&lifecycle_contract(), &evidence).is_empty());
    }

    #[test]
    fn lifecycle_protocol_abstains_on_incomplete_or_ambiguous_scope_evidence() {
        let contract = lifecycle_contract();
        let mut incomplete = lifecycle_evidence(&[(0, "request.close"), (1, "callback")]);
        incomplete.complete = false;
        assert!(validate_protocol_lifecycle(&contract, &incomplete).is_empty());

        let mut mixed_scope = lifecycle_evidence(&[(0, "request.close"), (1, "callback")]);
        mixed_scope.events[1].scope_id = "request-99".into();
        assert!(validate_protocol_lifecycle(&contract, &mixed_scope).is_empty());

        let duplicate_order = lifecycle_evidence(&[(0, "request.close"), (0, "callback")]);
        assert!(validate_protocol_lifecycle(&contract, &duplicate_order).is_empty());
    }
}
