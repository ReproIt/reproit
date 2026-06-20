//! Property-matched replay (tier 3): turn the cloud's `fixtureSpec` into
//! concrete, deterministic input data the runner types into matching fields,
//! so a bug that only hits SOME users (a 312-char unicode name, an emoji, a
//! Turkish dotless "i", an empty or RTL field, a specific locale) reproduces.
//!
//! The cloud derives the spec PII-safely from input fingerprints + cohort
//! discriminators (see `crates/cloud/src/ingest.rs::fixture_spec`); this module
//! is the reproit-side synthesizer. It generates FEATURES-matching values, not
//! the user's real data (which is never stored). Synthesis is deterministic
//! (no RNG): the same spec always yields the same fixture, so a property-matched
//! replay is as reproducible as an action replay.
//!
//! Spec shape (from the cloud):
//!   { "locale": "tr",
//!     "inputs": [ { "field": "name",
//!                   "generate": { "minLen": 312, "charset": "unicode",
//!                                 "emoji": true, "rtl": false, "empty": false } } ] }
//!
//! Output (written into `.reproit/fuzz_config.json`, read by the explorers):
//!   { "locale": "tr",
//!     "inputs": [ { "field": "name", "value": "<synthesized>" } ] }

use serde_json::{json, Map, Value};

/// One synthesized field: the a11y label/index to target and the value to type.
#[derive(Debug, Clone, PartialEq)]
pub struct FieldValue {
    pub field: String,
    pub value: String,
}

/// A synthesized fixture: the (best-effort) locale to drive plus the per-field
/// values. `locale` is any scalar discriminator the spec pinned as `locale`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Fixture {
    pub locale: Option<String>,
    pub fields: Vec<FieldValue>,
}

impl Fixture {
    /// True when there's nothing data-specific to synthesize: an action replay
    /// alone suffices and property-matched replay adds nothing.
    pub fn is_empty(&self) -> bool {
        self.locale.is_none() && self.fields.is_empty()
    }

    /// The `inputs`/`locale` object the explorers read out of fuzz_config.json.
    pub fn to_config(&self) -> Value {
        let mut m = Map::new();
        if let Some(loc) = &self.locale {
            m.insert("locale".to_string(), json!(loc));
        }
        let inputs: Vec<Value> = self
            .fields
            .iter()
            .map(|f| json!({ "field": f.field, "value": f.value }))
            .collect();
        m.insert("inputs".to_string(), json!(inputs));
        Value::Object(m)
    }

    /// A human-readable one-line summary for the CLI ("name<-312 unicode chars").
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if let Some(loc) = &self.locale {
            parts.push(format!("locale={loc}"));
        }
        for f in &self.fields {
            let v = &f.value;
            if v.is_empty() {
                parts.push(format!("{}=<empty>", f.field));
            } else {
                parts.push(format!("{}<-{} chars", f.field, v.chars().count()));
            }
        }
        if parts.is_empty() {
            "(nothing data-specific to synthesize)".to_string()
        } else {
            parts.join(", ")
        }
    }
}

/// Build a property-matched fixture from the cloud's `fixtureSpec`. Tolerates a
/// missing/empty/`{}` spec (returns an empty Fixture). Never panics on shape.
pub fn synthesize(spec: &Value) -> Fixture {
    let obj = match spec.as_object() {
        Some(o) => o,
        None => return Fixture::default(),
    };
    // `locale` is the canonical pinned dimension. Any other scalar string at the
    // top level (plan, role, ...) is captured too, but locale is what the
    // explorer can actually drive, so it is first-class.
    let locale = obj.get("locale").and_then(|v| v.as_str()).map(String::from);

    let fields = obj
        .get("inputs")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(field_from_directive).collect())
        .unwrap_or_default();

    Fixture { locale, fields }
}

/// Convert one `{ field, generate: {...} }` directive into a concrete value.
fn field_from_directive(d: &Value) -> Option<FieldValue> {
    let field = d.get("field").and_then(|v| v.as_str())?.to_string();
    let g = d.get("generate").and_then(|v| v.as_object());
    let value = match g {
        Some(g) => generate_value(g),
        None => String::new(),
    };
    Some(FieldValue { field, value })
}

/// The deterministic value generator. Honors `empty` (wins outright), `charset`
/// (numeric|ascii|unicode), `emoji`, `rtl`, and `minLen` (the value is grown by
/// repeating its base alphabet until it reaches at least `minLen` code points,
/// so length-sensitive bugs - overflow, truncation - reproduce).
fn generate_value(g: &Map<String, Value>) -> String {
    if g.get("empty").and_then(|v| v.as_bool()).unwrap_or(false) {
        return String::new();
    }
    let charset = g.get("charset").and_then(|v| v.as_str()).unwrap_or("ascii");
    let emoji = g.get("emoji").and_then(|v| v.as_bool()).unwrap_or(false);
    let rtl = g.get("rtl").and_then(|v| v.as_bool()).unwrap_or(false);
    let min_len = g.get("minLen").and_then(|v| v.as_u64()).unwrap_or(0) as usize;

    // Base alphabet by charset. The unicode base mixes scripts a real
    // problematic value would (Turkish dotless i, German esszett, accents,
    // CJK) so locale-folding and width bugs surface.
    let base: Vec<char> = if rtl {
        // Arabic + Hebrew letters: forces RTL layout and bidi handling.
        "اهلًاوسهلًاשלוםעולם".chars().collect()
    } else {
        match charset {
            "numeric" => "0123456789".chars().collect(),
            "unicode" => "ıİßçğşöüâéàü中文字测试".chars().collect(),
            // ascii (default): a realistic name-like mixed-case run.
            _ => "Aabcdefghijklmnopqrstuvwxyz".chars().collect(),
        }
    };

    let mut out: Vec<char> = Vec::new();
    if emoji {
        // Lead with an emoji (incl. a ZWJ-sequence family) so grapheme-cluster
        // and surrogate-pair bugs reproduce.
        out.extend("👩‍👩‍👧‍👦🚀".chars());
    }
    // Always lay down at least one base run so charset is represented even when
    // minLen is 0 or already covered by the emoji.
    let target = min_len.max(out.len() + base.len());
    let mut i = 0;
    while out.len() < target {
        out.push(base[i % base.len()]);
        i += 1;
    }
    out.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthesize_is_deterministic() {
        let spec = json!({
            "locale": "tr",
            "inputs": [{ "field": "name",
                "generate": { "minLen": 312, "charset": "unicode", "emoji": true } }]
        });
        let a = synthesize(&spec);
        let b = synthesize(&spec);
        assert_eq!(a, b, "same spec must yield byte-identical fixture");
        assert_eq!(a.locale.as_deref(), Some("tr"));
        assert_eq!(a.fields.len(), 1);
        assert_eq!(a.fields[0].field, "name");
        // minLen honored (code points, not bytes), emoji present.
        assert!(a.fields[0].value.chars().count() >= 312);
        assert!(a.fields[0].value.contains('🚀'));
        // Turkish dotless i is in the unicode base.
        assert!(a.fields[0].value.contains('ı'));
    }

    #[test]
    fn empty_flag_wins() {
        let spec = json!({ "inputs": [
            { "field": "bio", "generate": { "empty": true, "minLen": 50 } }
        ]});
        let f = synthesize(&spec);
        assert_eq!(f.fields[0].value, "");
        assert!(f.locale.is_none());
    }

    #[test]
    fn numeric_charset_is_digits_only() {
        let spec = json!({ "inputs": [
            { "field": "zip", "generate": { "charset": "numeric", "minLen": 20 } }
        ]});
        let f = synthesize(&spec);
        assert!(f.fields[0].value.chars().all(|c| c.is_ascii_digit()));
        assert!(f.fields[0].value.chars().count() >= 20);
    }

    #[test]
    fn rtl_produces_rtl_text() {
        let spec = json!({ "inputs": [
            { "field": "name", "generate": { "rtl": true, "minLen": 5 } }
        ]});
        let f = synthesize(&spec);
        // At least one strong-RTL char (Arabic U+0600..U+06FF or Hebrew U+0590..U+05FF).
        assert!(f.fields[0]
            .value
            .chars()
            .any(|c| ('\u{0590}'..='\u{06FF}').contains(&c)));
    }

    #[test]
    fn empty_spec_is_empty_fixture() {
        assert!(synthesize(&json!({})).is_empty());
        assert!(synthesize(&Value::Null).is_empty());
        assert!(synthesize(&json!({ "inputs": [] })).is_empty());
    }

    #[test]
    fn to_config_roundtrips_shape() {
        let f = synthesize(&json!({
            "locale": "de",
            "inputs": [{ "field": "name", "generate": { "charset": "ascii" } }]
        }));
        let cfg = f.to_config();
        assert_eq!(cfg["locale"], json!("de"));
        assert_eq!(cfg["inputs"][0]["field"], json!("name"));
        assert!(!cfg["inputs"][0]["value"].as_str().unwrap().is_empty());
    }

    #[test]
    fn missing_generate_yields_empty_value() {
        let f = synthesize(&json!({ "inputs": [{ "field": "x" }] }));
        assert_eq!(f.fields[0].value, "");
        assert!(!f.is_empty()); // a field with no value still pins that field
    }
}
