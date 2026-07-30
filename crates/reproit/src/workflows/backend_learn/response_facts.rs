//! Response facts shared by every family's source reader.
//!
//! `field_facts` is the vocabulary for what a handler ACCEPTS; this is the
//! vocabulary for what it RETURNS. A reader states, per handler, the status
//! codes its code names and the body it writes at each, as far as the type
//! system reaches and no further: a status behind an unreadable constant is
//! not stated, a body behind an untyped map is a body of unknown shape.

use std::collections::BTreeMap;

/// The wire shape of a value, as far as the source types state it.
///
/// `Unknown` is a first-class answer, not a failure: a `map[string]any` body
/// or an `Option<T>` field genuinely states nothing this can claim, and the
/// emitter renders it as an empty schema rather than a guess.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum WireShape {
    /// An OpenAPI primitive type name: string, integer, number, boolean.
    Primitive(&'static str),
    /// A JSON object whose fields the source does not enumerate (a map type).
    Object,
    Array(Box<WireShape>),
    /// A named serializer type, resolved against the declared structs at
    /// emission time; an unresolved or ambiguous name claims nothing.
    Named(String),
    Unknown,
}

/// One field of a serializer type as it appears on the wire.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct WireField {
    pub(super) shape: WireShape,
    /// Always present in the serialized output. Absent-able fields
    /// (`omitempty`, `skip_serializing_if`) are not required.
    pub(super) required: bool,
}

/// serializer type name -> its wire fields.
pub(super) type Serializers = BTreeMap<String, BTreeMap<String, WireField>>;

/// What one handler's code states it returns: each status it names, and the
/// body shape written at that status. An empty map states nothing.
#[derive(Debug, Clone, Default, PartialEq)]
pub(super) struct ResponseFact {
    pub(super) statuses: BTreeMap<u16, WireShape>,
}

impl ResponseFact {
    /// Record a status, never downgrading a stated body to `Unknown`: a
    /// chained `Status(201).JSON(v)` is seen once as the pair and once as the
    /// bare inner call, and the second sighting must not erase the first.
    pub(super) fn state(&mut self, status: u16, body: WireShape) {
        match self.statuses.get(&status) {
            Some(existing) if *existing != WireShape::Unknown && body == WireShape::Unknown => {}
            _ => {
                self.statuses.insert(status, body);
            }
        }
    }
}

/// The HTTP status a named constant states, or None for a name outside the
/// table: an unrecognised constant is a status this cannot claim.
///
/// Go writes `http.StatusOK`, Rust writes `StatusCode::OK` or actix's
/// `HttpResponse::Ok()`; normalizing case and separators lets one table serve
/// every family.
pub(super) fn named_status(name: &str) -> Option<u16> {
    let key: String = name
        .trim_start_matches("Status")
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase();
    let code = match key.as_str() {
        "ok" => 200,
        "created" => 201,
        "accepted" => 202,
        "nocontent" | "nonauthoritativeinfo" => 204,
        "movedpermanently" => 301,
        "found" => 302,
        "seeother" => 303,
        "notmodified" => 304,
        "temporaryredirect" => 307,
        "permanentredirect" => 308,
        "badrequest" => 400,
        "unauthorized" => 401,
        "paymentrequired" => 402,
        "forbidden" => 403,
        "notfound" => 404,
        "methodnotallowed" => 405,
        "notacceptable" => 406,
        "requesttimeout" => 408,
        "conflict" => 409,
        "gone" => 410,
        "preconditionfailed" => 412,
        "payloadtoolarge" | "requestentitytoolarge" => 413,
        "unsupportedmediatype" => 415,
        "unprocessableentity" => 422,
        "toomanyrequests" => 429,
        "internalservererror" => 500,
        "notimplemented" => 501,
        "badgateway" => 502,
        "serviceunavailable" => 503,
        "gatewaytimeout" => 504,
        _ => return None,
    };
    Some(code)
}

/// A status stated as a bare integer literal, bounds-checked to the HTTP
/// range so an unrelated number is not read as a status.
pub(super) fn literal_status(text: &str) -> Option<u16> {
    let code: u16 = text.trim().parse().ok()?;
    (100..=599).contains(&code).then_some(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_status_table_serves_go_and_rust_spellings() {
        assert_eq!(named_status("StatusOK"), Some(200));
        assert_eq!(named_status("OK"), Some(200));
        assert_eq!(named_status("Created"), Some(201));
        assert_eq!(named_status("CREATED"), Some(201));
        assert_eq!(named_status("INTERNAL_SERVER_ERROR"), Some(500));
        assert_eq!(named_status("StatusInternalServerError"), Some(500));
        assert_eq!(named_status("StatusTeapot"), None, "outside the table");
    }

    #[test]
    fn a_literal_status_must_be_in_the_http_range() {
        assert_eq!(literal_status("204"), Some(204));
        assert_eq!(literal_status("42"), None);
        assert_eq!(literal_status("nine"), None);
    }

    #[test]
    fn a_stated_body_is_never_downgraded_to_unknown() {
        let mut fact = ResponseFact::default();
        fact.state(201, WireShape::Named("Item".into()));
        fact.state(201, WireShape::Unknown);
        assert_eq!(fact.statuses[&201], WireShape::Named("Item".into()));
        fact.state(201, WireShape::Primitive("string"));
        assert_eq!(fact.statuses[&201], WireShape::Primitive("string"));
    }
}
