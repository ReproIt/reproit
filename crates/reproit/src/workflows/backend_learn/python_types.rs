//! Request-body facts read from Python handler signatures.
//!
//! Pydantic states more about a field than a Rust type does, and states it
//! declaratively: `Literal["user", "sponsor"]` is a closed value set,
//! `Field(ge=-1, le=1)` is a range, `Optional[T]` and a default are optionality.
//! There is no guard-reading needed here at all, which makes FastAPI and
//! django-ninja easier to check than the language this started with.

use super::rust_types::FieldFact;
use regex::Regex;
use std::collections::BTreeMap;

const MAX_TYPE_BYTES: usize = 2 * 1024 * 1024;

/// A model field as first read: name, annotation, and any default expression.
type RawField = (String, String, Option<String>);

#[derive(Debug, Default)]
pub(super) struct PythonTypes {
    /// handler fn -> body model name.
    bodies: BTreeMap<String, String>,
    /// model name -> field -> fact.
    models: BTreeMap<String, BTreeMap<String, FieldFact>>,
}

impl PythonTypes {
    pub(super) fn body_fields(&self, handler: &str) -> Option<BTreeMap<String, FieldFact>> {
        self.models.get(self.bodies.get(handler)?).cloned()
    }
}

pub(super) struct PythonScanner {
    model_open: Regex,
    enum_open: Regex,
    enum_member: Regex,
    field: Regex,
    def_open: Regex,
    parameter: Regex,
    literal: Regex,
    bound: Regex,
}

impl PythonScanner {
    pub(super) fn new(compile: impl Fn(&str) -> Regex) -> Self {
        Self {
            // `class BlockRequest(BaseModel):` and django-ninja's `Schema`.
            model_open: compile(r"^\s*class\s+([A-Za-z_]\w*)\s*\(([^)]*)\)\s*:"),
            enum_open: compile(r"^\s*class\s+([A-Za-z_]\w*)\s*\(([^)]*Enum[^)]*)\)\s*:"),
            enum_member: compile(r#"^\s+[A-Z_][A-Z0-9_]*\s*=\s*['"]([^'"]+)['"]"#),
            field: compile(r"^\s+([a-z_]\w*)\s*:\s*([^=]+?)\s*(?:=\s*(.+))?$"),
            def_open: compile(r"^\s*(?:async\s+)?def\s+([A-Za-z_]\w*)\s*\("),
            parameter: compile(r"([a-z_]\w*)\s*:\s*([A-Za-z_]\w*)"),
            literal: compile(r#"Literal\s*\[([^\]]*)\]"#),
            bound: compile(r"\b(ge|le|gt|lt)\s*=\s*(-?\d+(?:\.\d+)?)"),
        }
    }

    pub(super) fn scan(&self, sources: &[String]) -> PythonTypes {
        let mut types = PythonTypes::default();
        let mut enums: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut raw: BTreeMap<String, Vec<RawField>> = BTreeMap::new();
        for source in sources {
            if source.len() > MAX_TYPE_BYTES {
                continue;
            }
            self.scan_one(source, &mut types.bodies, &mut enums, &mut raw);
        }
        for (model, fields) in raw {
            let resolved = fields
                .into_iter()
                .map(|(name, annotation, default)| {
                    (name, self.fact(&annotation, default.as_deref(), &enums))
                })
                .collect();
            types.models.insert(model, resolved);
        }
        types
    }

    fn fact(
        &self,
        annotation: &str,
        default: Option<&str>,
        enums: &BTreeMap<String, Vec<String>>,
    ) -> FieldFact {
        // `Optional[T]`, `T | None`, and any default all mean the handler
        // accepts a body without the field.
        let optional = annotation.contains("Optional[")
            || annotation.contains("| None")
            || annotation.contains("None |")
            || default.is_some_and(|value| !value.contains("Field(") || value.contains("default"));
        let allowed = self
            .literal
            .captures(annotation)
            .and_then(|captures| literal_values(&captures[1]))
            .or_else(|| {
                enums
                    .iter()
                    .find(|(name, _)| annotation.contains(name.as_str()))
                    .map(|(_, values)| values.clone())
            });
        let range = default.and_then(|value| self.range(value));
        FieldFact {
            required: !optional,
            evidence: allowed
                .as_ref()
                .map(|_| "a Literal or Enum annotation".to_string())
                .or_else(|| range.map(|_| "a Field(...) bound".to_string())),
            allowed,
            range,
        }
    }

    /// `Field(ge=-1, le=1)` -> inclusive bounds. Exclusive `gt`/`lt` are
    /// converted, so the reported set is what the handler actually accepts.
    fn range(&self, default: &str) -> Option<(Option<f64>, Option<f64>)> {
        let mut low = None;
        let mut high = None;
        for captures in self.bound.captures_iter(default) {
            let value: f64 = captures[2].parse().ok()?;
            match &captures[1] {
                "ge" => low = Some(value),
                "gt" => low = Some(value + 1.0),
                "le" => high = Some(value),
                "lt" => high = Some(value - 1.0),
                _ => {}
            }
        }
        (low.is_some() || high.is_some()).then_some((low, high))
    }

    fn scan_one(
        &self,
        source: &str,
        bodies: &mut BTreeMap<String, String>,
        enums: &mut BTreeMap<String, Vec<String>>,
        models: &mut BTreeMap<String, Vec<RawField>>,
    ) {
        let lines: Vec<&str> = source.lines().map(strip_comment).collect();
        for (index, line) in lines.iter().enumerate() {
            if let Some(captures) = self.enum_open.captures(line) {
                let values: Vec<String> = indented_block(&lines, index)
                    .iter()
                    .filter_map(|member| self.enum_member.captures(member))
                    .map(|captures| captures[1].to_string())
                    .collect();
                if !values.is_empty() {
                    enums.insert(captures[1].to_string(), values);
                }
                continue;
            }
            if let Some(captures) = self.model_open.captures(line) {
                // Only declared request models: a plain class is not a body.
                if !captures[2].contains("BaseModel") && !captures[2].contains("Schema") {
                    continue;
                }
                let fields: Vec<RawField> = indented_block(&lines, index)
                    .iter()
                    .filter_map(|member| self.field.captures(member))
                    .map(|captures| {
                        (
                            captures[1].to_string(),
                            captures[2].trim().to_string(),
                            captures.get(3).map(|value| value.as_str().to_string()),
                        )
                    })
                    .collect();
                if !fields.is_empty() {
                    models.insert(captures[1].to_string(), fields);
                }
                continue;
            }
            if let Some(captures) = self.def_open.captures(line) {
                if let Some(model) = self.body_model(&lines, index, models) {
                    bodies.insert(captures[1].to_string(), model);
                }
            }
        }
    }

    /// The parameter annotated with a known request model. FastAPI infers the
    /// body from exactly that, so this reads the same signal the framework does.
    fn body_model(
        &self,
        lines: &[&str],
        at: usize,
        models: &BTreeMap<String, Vec<RawField>>,
    ) -> Option<String> {
        let mut signature = String::new();
        for line in lines.iter().skip(at).take(12) {
            signature.push_str(line);
            signature.push(' ');
            if line.contains(':') && line.trim_end().ends_with(':') {
                break;
            }
        }
        let open = signature.find('(')?;
        self.parameter
            .captures_iter(&signature[open..])
            .map(|captures| captures[2].to_string())
            .find(|name| models.contains_key(name))
    }
}

/// The lines of a Python block, by indentation: everything more indented than
/// the `class`/`def` line, up to the first line that is not.
fn indented_block<'a>(lines: &'a [&'a str], open: usize) -> Vec<&'a str> {
    let base = indent(lines[open]);
    let mut body = Vec::new();
    for line in lines.iter().skip(open + 1) {
        if line.trim().is_empty() {
            continue;
        }
        if indent(line) <= base {
            break;
        }
        body.push(*line);
    }
    body
}

fn indent(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

fn strip_comment(line: &str) -> &str {
    match line.find('#') {
        Some(at) => &line[..at],
        None => line,
    }
}

/// `"user", "sponsor"` -> the values, or None if any item is not a literal.
fn literal_values(inner: &str) -> Option<Vec<String>> {
    let mut values = Vec::new();
    for part in inner.split(',') {
        let item = part.trim();
        if item.is_empty() {
            continue;
        }
        let unquoted = item.trim_matches(['"', '\'']);
        if unquoted == item && item.parse::<f64>().is_err() {
            return None;
        }
        values.push(unquoted.to_string());
    }
    (values.len() > 1).then_some(values)
}

/// Read every Python source under `root` and resolve its request-body models.
pub(super) fn scan_types(root: &std::path::Path) -> PythonTypes {
    let scanner = PythonScanner::new(|pattern| Regex::new(pattern).expect("static python pattern"));
    let sources: Vec<String> = super::extract::family_sources(root, super::extract::Family::Python)
        .into_iter()
        .filter_map(|file| std::fs::read_to_string(file).ok())
        .collect();
    scanner.scan(&sources)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan(sources: &[&str]) -> PythonTypes {
        PythonScanner::new(|p| Regex::new(p).expect("pattern"))
            .scan(&sources.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    const APP: &str = r#"
class BlockRequest(BaseModel):
    blocked_type: Literal["user", "sponsor"]
    blocked_id: str
    note: Optional[str] = None
    rating: int = Field(ge=-1, le=1)

@app.post("/v1/blocks")
async def create_block(body: BlockRequest):
    return {}
"#;

    #[test]
    fn a_literal_annotation_is_a_closed_value_set() {
        let fields = scan(&[APP]).body_fields("create_block").expect("resolved");
        assert_eq!(
            fields["blocked_type"].allowed.as_deref(),
            Some(["user".to_string(), "sponsor".to_string()].as_slice())
        );
        assert!(fields["blocked_type"].required);
    }

    #[test]
    fn optional_and_defaulted_fields_are_not_required() {
        let fields = scan(&[APP]).body_fields("create_block").unwrap();
        assert!(!fields["note"].required);
        assert!(fields["blocked_id"].required);
    }

    #[test]
    fn a_field_bound_is_a_range() {
        let fields = scan(&[APP]).body_fields("create_block").unwrap();
        assert_eq!(fields["rating"].range, Some((Some(-1.0), Some(1.0))));
        assert!(
            fields["rating"].required,
            "Field(...) with no default is required"
        );
    }

    #[test]
    fn a_str_enum_class_is_a_closed_value_set() {
        let types = scan(&[r#"
class BlockedType(str, Enum):
    USER = "user"
    SPONSOR = "sponsor"

class R(BaseModel):
    blocked_type: BlockedType

@app.post("/x")
def h(body: R):
    return {}
"#]);
        assert_eq!(
            types.body_fields("h").unwrap()["blocked_type"]
                .allowed
                .as_deref(),
            Some(["user".to_string(), "sponsor".to_string()].as_slice())
        );
    }

    #[test]
    fn a_plain_class_is_not_treated_as_a_request_body() {
        let types = scan(&[r#"
class Helper:
    thing: str

@app.post("/x")
def h(body: Helper):
    return {}
"#]);
        assert!(
            types.body_fields("h").is_none(),
            "only declared models are bodies"
        );
    }

    #[test]
    fn an_open_annotation_stays_open() {
        let fields = scan(&[APP]).body_fields("create_block").unwrap();
        assert_eq!(fields["blocked_id"].allowed, None);
        assert_eq!(fields["blocked_id"].range, None);
    }

    #[test]
    fn exclusive_bounds_convert_to_what_the_handler_accepts() {
        let types = scan(&[r#"
class R(BaseModel):
    n: int = Field(gt=0, lt=10)

@app.post("/x")
def h(body: R):
    return {}
"#]);
        assert_eq!(
            types.body_fields("h").unwrap()["n"].range,
            Some((Some(1.0), Some(9.0)))
        );
    }
}
