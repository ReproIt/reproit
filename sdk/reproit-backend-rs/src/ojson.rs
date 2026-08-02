//! Order-preserving JSON for the replay path (feature `instrument`).
//!
//! The `REPROIT:DIVERGENCE` marker must be BYTE-identical to the Node
//! reference, and Node serializes objects in insertion/parse order while
//! serde_json's map is a BTreeMap that sorts keys. Enabling serde_json's
//! `preserve_order` here would leak through cargo feature unification into
//! every workspace build and silently reorder the canonical (sorted-key)
//! wire encoding, so the replay path carries its own small ordered value
//! instead: parse keeps document order, serialization is compact, and
//! number tokens are kept verbatim so a parse/serialize round trip of a
//! compact document returns its exact bytes.

/// One JSON value with object key order preserved. Numbers keep their raw
/// token (for byte-stable re-serialization) plus the parsed f64 (for the
/// equality the matcher needs, where 5 == 5.0 exactly as in Node/Python).
#[derive(Debug, Clone)]
pub enum OValue {
    Null,
    Bool(bool),
    Num(f64, String),
    Str(String),
    Arr(Vec<OValue>),
    Obj(Vec<(String, OValue)>),
}

/// Parse nesting bound: fail closed on adversarially deep documents instead
/// of overflowing the stack.
const MAX_DEPTH: usize = 128;

impl OValue {
    pub fn num(value: u64) -> Self {
        OValue::Num(value as f64, value.to_string())
    }

    pub fn get(&self, key: &str) -> Option<&OValue> {
        match self {
            OValue::Obj(fields) => fields
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            OValue::Str(text) => Some(text),
            _ => None,
        }
    }

    pub fn as_u64(&self) -> Option<u64> {
        match self {
            OValue::Num(value, _) if *value >= 0.0 && value.fract() == 0.0 => Some(*value as u64),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            OValue::Bool(value) => Some(*value),
            _ => None,
        }
    }

    pub fn as_arr(&self) -> Option<&[OValue]> {
        match self {
            OValue::Arr(items) => Some(items),
            _ => None,
        }
    }

    /// Compact serialization, byte-compatible with `JSON.stringify` of the
    /// same parse (insertion order, no spaces, verbatim number tokens).
    pub fn to_compact(&self) -> String {
        let mut out = String::new();
        self.write(&mut out);
        out
    }

    fn write(&self, out: &mut String) {
        match self {
            OValue::Null => out.push_str("null"),
            OValue::Bool(true) => out.push_str("true"),
            OValue::Bool(false) => out.push_str("false"),
            OValue::Num(_, token) => out.push_str(token),
            OValue::Str(text) => write_string(text, out),
            OValue::Arr(items) => {
                out.push('[');
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    item.write(out);
                }
                out.push(']');
            }
            OValue::Obj(fields) => {
                out.push('{');
                for (index, (name, value)) in fields.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    write_string(name, out);
                    out.push(':');
                    value.write(out);
                }
                out.push('}');
            }
        }
    }

    /// Convert to a serde value for callers that need the sorted-key world
    /// (db outcomes handed back to the application).
    pub fn to_serde(&self) -> serde_json::Value {
        match self {
            OValue::Null => serde_json::Value::Null,
            OValue::Bool(value) => serde_json::Value::Bool(*value),
            OValue::Num(value, token) => {
                serde_json::from_str(token).unwrap_or_else(|_| serde_json::json!(value))
            }
            OValue::Str(text) => serde_json::Value::String(text.clone()),
            OValue::Arr(items) => {
                serde_json::Value::Array(items.iter().map(OValue::to_serde).collect())
            }
            OValue::Obj(fields) => serde_json::Value::Object(
                fields
                    .iter()
                    .map(|(name, value)| (name.clone(), value.to_serde()))
                    .collect(),
            ),
        }
    }
}

/// Scalar equality as Node's `===` / Python's `==` see it: numbers compare
/// by value, everything else by kind and content. Objects and arrays are
/// NOT compared here; the matcher recurses those itself.
pub fn scalar_eq(left: &OValue, right: &OValue) -> bool {
    match (left, right) {
        (OValue::Null, OValue::Null) => true,
        (OValue::Bool(a), OValue::Bool(b)) => a == b,
        (OValue::Num(a, _), OValue::Num(b, _)) => a == b,
        (OValue::Str(a), OValue::Str(b)) => a == b,
        _ => false,
    }
}

fn write_string(text: &str, out: &mut String) {
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            ch if (ch as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", ch as u32));
            }
            ch => out.push(ch),
        }
    }
    out.push('"');
}

pub fn parse(text: &str) -> Option<OValue> {
    let bytes = text.as_bytes();
    let mut at = 0;
    let value = parse_value(bytes, &mut at, 0)?;
    skip_ws(bytes, &mut at);
    (at == bytes.len()).then_some(value)
}

fn skip_ws(bytes: &[u8], at: &mut usize) {
    while *at < bytes.len() && matches!(bytes[*at], b' ' | b'\t' | b'\n' | b'\r') {
        *at += 1;
    }
}

fn parse_value(bytes: &[u8], at: &mut usize, depth: usize) -> Option<OValue> {
    if depth > MAX_DEPTH {
        return None;
    }
    skip_ws(bytes, at);
    match *bytes.get(*at)? {
        b'{' => parse_obj(bytes, at, depth),
        b'[' => parse_arr(bytes, at, depth),
        b'"' => parse_string(bytes, at).map(OValue::Str),
        b't' => parse_lit(bytes, at, "true", OValue::Bool(true)),
        b'f' => parse_lit(bytes, at, "false", OValue::Bool(false)),
        b'n' => parse_lit(bytes, at, "null", OValue::Null),
        _ => parse_num(bytes, at),
    }
}

fn parse_lit(bytes: &[u8], at: &mut usize, literal: &str, value: OValue) -> Option<OValue> {
    if bytes[*at..].starts_with(literal.as_bytes()) {
        *at += literal.len();
        return Some(value);
    }
    None
}

fn parse_num(bytes: &[u8], at: &mut usize) -> Option<OValue> {
    let start = *at;
    if bytes.get(*at) == Some(&b'-') {
        *at += 1;
    }
    while *at < bytes.len() && matches!(bytes[*at], b'0'..=b'9' | b'.' | b'e' | b'E' | b'+' | b'-')
    {
        *at += 1;
    }
    let token = std::str::from_utf8(&bytes[start..*at]).ok()?;
    token
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite())
        .map(|value| OValue::Num(value, token.to_string()))
}

fn parse_string(bytes: &[u8], at: &mut usize) -> Option<String> {
    if bytes.get(*at) != Some(&b'"') {
        return None;
    }
    *at += 1;
    let mut out = String::new();
    loop {
        match *bytes.get(*at)? {
            b'"' => {
                *at += 1;
                return Some(out);
            }
            b'\\' => {
                *at += 1;
                match *bytes.get(*at)? {
                    b'"' => out.push('"'),
                    b'\\' => out.push('\\'),
                    b'/' => out.push('/'),
                    b'b' => out.push('\u{8}'),
                    b'f' => out.push('\u{c}'),
                    b'n' => out.push('\n'),
                    b'r' => out.push('\r'),
                    b't' => out.push('\t'),
                    b'u' => {
                        let high = parse_hex4(bytes, at)?;
                        let code = if (0xd800..0xdc00).contains(&high)
                            && bytes.get(*at + 1..*at + 3) == Some(b"\\u")
                        {
                            *at += 2;
                            let low = parse_hex4(bytes, at)?;
                            0x10000 + ((high - 0xd800) << 10) + (low - 0xdc00)
                        } else {
                            high
                        };
                        out.push(char::from_u32(code).unwrap_or('\u{fffd}'));
                    }
                    _ => return None,
                }
                *at += 1;
            }
            _ => {
                // Consume one UTF-8 character (bytes are valid: input is &str).
                let rest = std::str::from_utf8(&bytes[*at..]).ok()?;
                let ch = rest.chars().next()?;
                out.push(ch);
                *at += ch.len_utf8();
            }
        }
    }
}

/// Reads the 4 hex digits after `\u`; leaves `at` on the last digit.
fn parse_hex4(bytes: &[u8], at: &mut usize) -> Option<u32> {
    let digits = bytes.get(*at + 1..*at + 5)?;
    let code = u32::from_str_radix(std::str::from_utf8(digits).ok()?, 16).ok()?;
    *at += 4;
    Some(code)
}

fn parse_arr(bytes: &[u8], at: &mut usize, depth: usize) -> Option<OValue> {
    *at += 1;
    let mut items = Vec::new();
    skip_ws(bytes, at);
    if bytes.get(*at) == Some(&b']') {
        *at += 1;
        return Some(OValue::Arr(items));
    }
    loop {
        items.push(parse_value(bytes, at, depth + 1)?);
        skip_ws(bytes, at);
        match *bytes.get(*at)? {
            b',' => *at += 1,
            b']' => {
                *at += 1;
                return Some(OValue::Arr(items));
            }
            _ => return None,
        }
    }
}

fn parse_obj(bytes: &[u8], at: &mut usize, depth: usize) -> Option<OValue> {
    *at += 1;
    let mut fields = Vec::new();
    skip_ws(bytes, at);
    if bytes.get(*at) == Some(&b'}') {
        *at += 1;
        return Some(OValue::Obj(fields));
    }
    loop {
        skip_ws(bytes, at);
        let name = parse_string(bytes, at)?;
        skip_ws(bytes, at);
        if bytes.get(*at) != Some(&b':') {
            return None;
        }
        *at += 1;
        fields.push((name, parse_value(bytes, at, depth + 1)?));
        skip_ws(bytes, at);
        match *bytes.get(*at)? {
            b',' => *at += 1,
            b'}' => {
                *at += 1;
                return Some(OValue::Obj(fields));
            }
            _ => return None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_round_trip_preserves_order_and_bytes() {
        let text = r#"{"method":"GET","url":"http://x/y?a=1","body":{"z":1,"a":[true,null,2.5]}}"#;
        assert_eq!(parse(text).expect("parse").to_compact(), text);
    }

    #[test]
    fn escapes_and_unicode_round_trip() {
        let parsed = parse(r#"["a\nb","A","😀","café"]"#).expect("parse");
        // Re-serialization decodes escapes the way JSON.stringify would.
        assert_eq!(
            parsed.to_compact(),
            "[\"a\\nb\",\"A\",\"\u{1f600}\",\"caf\u{e9}\"]"
        );
    }

    #[test]
    fn depth_and_trailing_garbage_fail_closed() {
        let deep = "[".repeat(200) + &"]".repeat(200);
        assert!(parse(&deep).is_none());
        assert!(parse("{} trailing").is_none());
        assert!(parse("{\"a\":}").is_none());
    }

    #[test]
    fn numbers_compare_by_value_and_serialize_verbatim() {
        let five = parse("5").expect("5");
        let five_f = parse("5.0").expect("5.0");
        assert!(scalar_eq(&five, &five_f));
        assert_eq!(five_f.to_compact(), "5.0");
    }
}
