//! Request-body facts read from Rust handler signatures.
//!
//! Checking paths caught the schema that points at a route nobody serves. It
//! does not catch the far more expensive case: a path that IS served, whose
//! declared TYPES the handler rejects. `blocked_type: {type: string}` against a
//! handler taking an enum costs 100% of that operation's mutations, every one a
//! 400, while the run still reports the operation as exercised.
//!
//! What can honestly be read from source is narrow, and this stays inside it:
//!
//! - a field whose Rust type is a UNIT-ONLY enum has a closed value set, so a
//!   schema declaring it an open `string` will generate values the handler
//!   refuses;
//! - a field the schema declares that the struct does not have is as dead as a
//!   wrong path;
//! - a non-`Option` field the schema does not mark required will be omitted by
//!   generation and 400.
//!
//! Everything else abstains. A `rating: i8` the handler range-checks at runtime
//! is invisible here, and guessing at it would be exactly the overclaiming the
//! schema is guilty of.

use regex::Regex;
use std::collections::BTreeMap;

/// Bound the source considered, matching the extractor's own walk limits.
const MAX_TYPE_BYTES: usize = 2 * 1024 * 1024;

/// What the code says about one request-body field.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct FieldFact {
    /// Non-`Option`, so the handler will reject a body that omits it.
    pub(super) required: bool,
    /// The exact values a unit-only enum accepts, serde renames applied. None
    /// when the type is open (a String, a number, another struct).
    pub(super) allowed: Option<Vec<String>>,
}

/// Request-body facts per handler function.
#[derive(Debug, Default)]
pub(super) struct RustTypes {
    /// handler fn -> body type name.
    bodies: BTreeMap<String, String>,
    /// struct name -> field name -> fact.
    structs: BTreeMap<String, BTreeMap<String, FieldFact>>,
}

impl RustTypes {
    /// The body fields a handler accepts, if its body type could be resolved.
    pub(super) fn body_fields(&self, handler: &str) -> Option<&BTreeMap<String, FieldFact>> {
        self.structs.get(self.bodies.get(handler)?)
    }
}

pub(super) struct TypeScanner {
    handler_body: Regex,
    enum_open: Regex,
    struct_open: Regex,
    field: Regex,
    rename_all: Regex,
    rename: Regex,
    variant: Regex,
}

impl TypeScanner {
    pub(super) fn new(compile: impl Fn(&str) -> Regex) -> Self {
        Self {
            // `async fn create(..., Json(body): Json<CreateRequest>)`. Axum's
            // body extractor is always last, but the signature may wrap, so the
            // whole signature is searched rather than one line.
            handler_body: compile(r"\bfn\s+([A-Za-z_][A-Za-z0-9_]*)"),
            enum_open: compile(r"\benum\s+([A-Za-z_][A-Za-z0-9_]*)"),
            struct_open: compile(r"\bstruct\s+([A-Za-z_][A-Za-z0-9_]*)"),
            field: compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?([a-z_][a-z0-9_]*)\s*:\s*(.+?)\s*$"),
            rename_all: compile(r#"rename_all\s*=\s*"([^"]+)""#),
            rename: compile(r#"rename\s*=\s*"([^"]+)""#),
            variant: compile(r"^[A-Z][A-Za-z0-9_]*$"),
        }
    }

    pub(super) fn scan(&self, sources: &[String]) -> RustTypes {
        let mut types = RustTypes::default();
        let mut enums: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut raw_structs: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
        for source in sources {
            if source.len() > MAX_TYPE_BYTES {
                continue;
            }
            self.scan_one(source, &mut types.bodies, &mut enums, &mut raw_structs);
        }
        // Resolve field types against the enums, now that every file is read: a
        // struct and the enum it uses are usually in different modules.
        for (name, fields) in raw_structs {
            let resolved = fields
                .into_iter()
                .map(|(field, declared)| {
                    let optional = declared.starts_with("Option<");
                    let inner = declared
                        .strip_prefix("Option<")
                        .and_then(|rest| rest.strip_suffix('>'))
                        .unwrap_or(&declared)
                        .trim()
                        .rsplit("::")
                        .next()
                        .unwrap_or("")
                        .to_string();
                    (
                        field,
                        FieldFact {
                            required: !optional,
                            allowed: enums.get(&inner).cloned(),
                        },
                    )
                })
                .collect();
            types.structs.insert(name, resolved);
        }
        types
    }

    fn scan_one(
        &self,
        source: &str,
        bodies: &mut BTreeMap<String, String>,
        enums: &mut BTreeMap<String, Vec<String>>,
        structs: &mut BTreeMap<String, Vec<(String, String)>>,
    ) {
        let lines: Vec<&str> = source.lines().map(strip_comment).collect();
        for (index, line) in lines.iter().enumerate() {
            if let Some(captures) = self.enum_open.captures(line) {
                if let Some(values) = self.unit_variants(&lines, index) {
                    enums.insert(captures[1].to_string(), values);
                }
            }
            if let Some(captures) = self.struct_open.captures(line) {
                let fields = self.named_fields(&lines, index);
                if !fields.is_empty() {
                    structs.insert(captures[1].to_string(), fields);
                }
            }
            if let Some(captures) = self.handler_body.captures(line) {
                let name = captures[1].to_string();
                if let Some(body) = self.body_type(&lines, index) {
                    bodies.insert(name, body);
                }
            }
        }
    }

    /// The serde-visible values of a UNIT-ONLY enum. Any variant carrying a
    /// payload makes the set open as far as this check is concerned, so it
    /// abstains rather than reporting a value set the handler does not have.
    fn unit_variants(&self, lines: &[&str], open: usize) -> Option<Vec<String>> {
        let rename_all = lines[open.saturating_sub(4)..=open]
            .iter()
            .find_map(|line| self.rename_all.captures(line))
            .map(|captures| captures[1].to_string());
        let body = block_text(lines, open)?;
        let mut values = Vec::new();
        for item in split_top_level(&body) {
            let (attributes, name) = split_attributes(&item);
            if name.is_empty() {
                continue;
            }
            // `User(Uuid)` or `Custom { id: Uuid }`: not a plain value.
            if !self.variant.is_match(&name) {
                return None;
            }
            let renamed = attributes
                .iter()
                .find_map(|attribute| self.rename.captures(attribute))
                .map(|captures| captures[1].to_string());
            values.push(match renamed {
                Some(renamed) => renamed,
                None => apply_rename_all(&name, rename_all.as_deref()),
            });
        }
        (!values.is_empty()).then_some(values)
    }

    fn named_fields(&self, lines: &[&str], open: usize) -> Vec<(String, String)> {
        let Some(body) = block_text(lines, open) else {
            return Vec::new();
        };
        let mut fields = Vec::new();
        for item in split_top_level(&body) {
            let (attributes, declaration) = split_attributes(&item);
            let Some(captures) = self.field.captures(&declaration) else {
                continue;
            };
            let renamed = attributes
                .iter()
                .find_map(|attribute| self.rename.captures(attribute))
                .map(|captures| captures[1].to_string());
            let name = renamed.unwrap_or_else(|| captures[1].to_string());
            fields.push((name, captures[2].trim().to_string()));
        }
        fields
    }

    /// The `Json<T>` REQUEST body type of a handler.
    ///
    /// Searched inside the signature's parentheses only. A handler that RETURNS
    /// `Json<Vec<Row>>` has no declared request body, and reading the return
    /// type as one would invent fields the endpoint never accepts.
    fn body_type(&self, lines: &[&str], at: usize) -> Option<String> {
        let joined = joined_from(lines, at);
        let open = joined.find('(')?;
        let mut depth = 0i32;
        let mut close = None;
        for (index, character) in joined[open..].char_indices() {
            match character {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        close = Some(open + index);
                        break;
                    }
                }
                _ => {}
            }
        }
        let parameters = &joined[open..close?];
        let start = parameters.find("Json<")? + "Json<".len();
        let rest = &parameters[start..];
        let end = rest.find('>')?;
        let name = rest[..end].trim().rsplit("::").next()?.to_string();
        (!name.is_empty()).then_some(name)
    }
}

/// Bound how far a single declaration may span, so a malformed file cannot turn
/// the scan quadratic.
const MAX_DECLARATION_LINES: usize = 400;

fn joined_from(lines: &[&str], at: usize) -> String {
    let end = (at + MAX_DECLARATION_LINES).min(lines.len());
    lines[at..end].join("\n")
}

/// The text between the braces of the declaration starting at `open`, following
/// nesting, whether the body is on the same line or many.
fn block_text(lines: &[&str], open: usize) -> Option<String> {
    let joined = joined_from(lines, open);
    let start = joined.find('{')?;
    let mut depth = 0i32;
    for (index, character) in joined[start..].char_indices() {
        match character {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(joined[start + 1..start + index].to_string());
                }
            }
            _ => {}
        }
    }
    None
}

/// Split a declaration body on commas that are not inside brackets, so
/// `Vec<A, B>` and `Custom { a, b }` stay one item.
fn split_top_level(body: &str) -> Vec<String> {
    let mut items = Vec::new();
    let mut depth = 0i32;
    let mut current = String::new();
    for character in body.chars() {
        match character {
            '<' | '(' | '[' | '{' => depth += 1,
            '>' | ')' | ']' | '}' => depth -= 1,
            ',' if depth == 0 => {
                items.push(std::mem::take(&mut current));
                continue;
            }
            _ => {}
        }
        current.push(character);
    }
    if !current.trim().is_empty() {
        items.push(current);
    }
    items
        .into_iter()
        .filter(|item| !item.trim().is_empty())
        .collect()
}

/// Separate leading `#[...]` attributes from the declaration they annotate.
fn split_attributes(item: &str) -> (Vec<String>, String) {
    let mut attributes = Vec::new();
    let mut rest = item.trim();
    while rest.starts_with("#[") {
        let mut depth = 0i32;
        let mut end = None;
        for (index, character) in rest.char_indices() {
            match character {
                '[' => depth += 1,
                ']' => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(index + 1);
                        break;
                    }
                }
                _ => {}
            }
        }
        let Some(end) = end else { break };
        attributes.push(rest[..end].to_string());
        rest = rest[end..].trim();
    }
    (attributes, rest.trim().to_string())
}

fn strip_comment(line: &str) -> &str {
    match line.find("//") {
        Some(at) => &line[..at],
        None => line,
    }
}

fn apply_rename_all(variant: &str, rule: Option<&str>) -> String {
    match rule {
        Some("snake_case") => to_snake(variant),
        Some("SCREAMING_SNAKE_CASE") => to_snake(variant).to_uppercase(),
        Some("kebab-case") => to_snake(variant).replace('_', "-"),
        Some("lowercase") => variant.to_lowercase(),
        Some("UPPERCASE") => variant.to_uppercase(),
        Some("camelCase") => {
            let snake = to_snake(variant);
            let mut parts = snake.split('_');
            let first = parts.next().unwrap_or("").to_string();
            first
                + &parts
                    .map(|part| {
                        let mut chars = part.chars();
                        match chars.next() {
                            Some(head) => head.to_uppercase().to_string() + chars.as_str(),
                            None => String::new(),
                        }
                    })
                    .collect::<String>()
        }
        // No rule: serde keeps the variant name verbatim.
        _ => variant.to_string(),
    }
}

fn to_snake(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 4);
    for (index, character) in value.chars().enumerate() {
        if character.is_uppercase() {
            if index > 0 {
                out.push('_');
            }
            out.extend(character.to_lowercase());
        } else {
            out.push(character);
        }
    }
    out
}

/// Read every Rust source under `root` and resolve its request-body types.
/// Reuses the extractor's bounded, deterministic walk so the type check sees
/// exactly the files the route check saw.
pub(super) fn scan_types(root: &std::path::Path) -> RustTypes {
    let scanner = TypeScanner::new(|pattern| Regex::new(pattern).expect("static type pattern"));
    let sources: Vec<String> = super::extract::rust_sources(root)
        .into_iter()
        .filter_map(|file| std::fs::read_to_string(file).ok())
        .collect();
    scanner.scan(&sources)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scanner() -> TypeScanner {
        TypeScanner::new(|pattern| Regex::new(pattern).expect("valid pattern"))
    }

    fn scan(sources: &[&str]) -> RustTypes {
        scanner().scan(&sources.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn a_unit_only_enum_field_reports_its_closed_value_set() {
        // The reported case, exactly: the schema said `string`, the handler
        // takes an enum, and every generated value was rejected.
        let types = scan(&[
            r#"
            #[derive(Deserialize)]
            #[serde(rename_all = "snake_case")]
            pub enum BlockedType { User, Sponsor }
            "#,
            r#"
            #[derive(Deserialize)]
            pub struct BlockRequest {
                pub blocked_type: BlockedType,
                pub note: Option<String>,
            }
            "#,
            r#"pub async fn create_block(Json(body): Json<BlockRequest>) -> impl IntoResponse {"#,
        ]);
        let fields = types.body_fields("create_block").expect("resolved body");
        assert_eq!(
            fields["blocked_type"].allowed.as_deref(),
            Some(["user".to_string(), "sponsor".to_string()].as_slice())
        );
        assert!(fields["blocked_type"].required);
        assert_eq!(fields["note"].allowed, None, "a String is an open type");
        assert!(!fields["note"].required, "Option means optional");
    }

    #[test]
    fn an_enum_with_a_payload_abstains_rather_than_claiming_a_closed_set() {
        let types = scan(&[
            "pub enum Target { User(Uuid), Everyone }",
            "pub struct R { pub target: Target }",
            "async fn h(Json(b): Json<R>) {",
        ]);
        let fields = types.body_fields("h").expect("resolved");
        assert_eq!(
            fields["target"].allowed, None,
            "a data-carrying variant is not a closed value set"
        );
    }

    #[test]
    fn per_variant_and_container_serde_renames_are_honoured() {
        let types = scan(&[
            r#"
            #[serde(rename_all = "SCREAMING_SNAKE_CASE")]
            pub enum Mode { FastPath, #[serde(rename = "slow")] SlowPath }
            "#,
            "pub struct R { pub mode: Mode }",
            "async fn h(Json(b): Json<R>) {",
        ]);
        let allowed = types.body_fields("h").unwrap()["mode"].allowed.clone();
        assert_eq!(
            allowed,
            Some(vec!["FAST_PATH".to_string(), "slow".to_string()])
        );
    }

    #[test]
    fn a_handler_with_no_json_body_resolves_nothing() {
        let types = scan(&["async fn list(State(db): State<Db>) -> Json<Vec<Row>> {"]);
        assert!(
            types.body_fields("list").is_none(),
            "a Json RETURN type is not a request body"
        );
    }

    #[test]
    fn a_wrapped_signature_still_resolves() {
        let types = scan(&[
            "pub struct R { pub a: String }",
            r#"
            pub async fn create(
                State(db): State<Db>,
                Json(body): Json<R>,
            ) -> impl IntoResponse {
            "#,
        ]);
        assert!(types.body_fields("create").is_some());
    }

    #[test]
    fn rename_all_rules_map_as_serde_does() {
        assert_eq!(
            apply_rename_all("FastPath", Some("snake_case")),
            "fast_path"
        );
        assert_eq!(
            apply_rename_all("FastPath", Some("kebab-case")),
            "fast-path"
        );
        assert_eq!(apply_rename_all("FastPath", Some("camelCase")), "fastPath");
        assert_eq!(apply_rename_all("FastPath", None), "FastPath");
    }
}
