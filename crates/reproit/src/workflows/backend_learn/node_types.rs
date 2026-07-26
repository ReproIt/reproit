//! Request-body facts read from JavaScript and TypeScript sources.
//!
//! Three declarative shapes cover most of the ecosystem: a zod object schema, a
//! TypeScript union of string literals, and NestJS class-validator decorators.
//! Each states its accepted set outright, so none of them needs inference.

use super::rust_types::FieldFact;
use regex::Regex;
use std::collections::BTreeMap;

const MAX_TYPE_BYTES: usize = 2 * 1024 * 1024;
/// How far past a declaration a body may run before it is treated as unreadable.
const MAX_BLOCK_LINES: usize = 400;

#[derive(Debug, Default)]
pub(super) struct NodeTypes {
    /// zod schema / DTO class / interface name -> fields.
    shapes: BTreeMap<String, BTreeMap<String, FieldFact>>,
    /// handler fn -> the shape it validates against.
    bodies: BTreeMap<String, String>,
}

impl NodeTypes {
    /// A route names either the handler or the schema it is wrapped in, so both
    /// are tried as keys.
    pub(super) fn body_fields(&self, key: &str) -> Option<BTreeMap<String, FieldFact>> {
        if let Some(fields) = self.shapes.get(key) {
            return Some(fields.clone());
        }
        self.shapes.get(self.bodies.get(key)?).cloned()
    }
}

pub(super) struct NodeScanner {
    zod_object: Regex,
    zod_field: Regex,
    zod_enum: Regex,
    zod_bound: Regex,
    union_alias: Regex,
    interface_open: Regex,
    interface_field: Regex,
    class_open: Regex,
    decorator: Regex,
    class_field: Regex,
    function_open: Regex,
    parse_call: Regex,
}

impl NodeScanner {
    pub(super) fn new(compile: impl Fn(&str) -> Regex) -> Self {
        Self {
            zod_object: compile(
                r"(?:const|let|var)\s+([A-Za-z_$][\w$]*)\s*=\s*z\s*\.\s*object\s*\(",
            ),
            zod_field: compile(r"^\s*([A-Za-z_$][\w$]*)\s*:\s*(z\s*\..+?),?\s*$"),
            zod_enum: compile(r"\.\s*enum\s*\(\s*\[([^\]]*)\]"),
            zod_bound: compile(r"\.\s*(min|max)\s*\(\s*(-?\d+(?:\.\d+)?)"),
            // `type Blocked = 'user' | 'sponsor'`
            union_alias: compile(r"type\s+([A-Za-z_$][\w$]*)\s*=\s*([^;]+)"),
            interface_open: compile(r"(?:interface|type)\s+([A-Za-z_$][\w$]*)\s*(?:=\s*)?\{"),
            interface_field: compile(r"^\s*([A-Za-z_$][\w$]*)(\??)\s*:\s*([^;,]+)"),
            class_open: compile(r"class\s+([A-Za-z_$][\w$]*)"),
            decorator: compile(r"@(IsIn|Min|Max|IsOptional|IsNotEmpty|IsDefined)\s*\(([^)]*)\)"),
            class_field: compile(r"^\s*(?:readonly\s+)?([A-Za-z_$][\w$]*)(\??)\s*:"),
            function_open: compile(
                r"(?:function\s+([A-Za-z_$][\w$]*)|(?:const|let)\s+([A-Za-z_$][\w$]*)\s*=\s*(?:async\s*)?\()",
            ),
            parse_call: compile(r"([A-Za-z_$][\w$]*)\s*\.\s*(?:safeParse|parse|parseAsync)\s*\("),
        }
    }

    pub(super) fn scan(&self, sources: &[String]) -> NodeTypes {
        let mut types = NodeTypes::default();
        let mut unions: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for source in sources {
            if source.len() > MAX_TYPE_BYTES {
                continue;
            }
            self.scan_one(source, &mut types, &mut unions);
        }
        // Resolve interface fields whose type is a declared string union.
        for fields in types.shapes.values_mut() {
            for fact in fields.values_mut() {
                // Only an unresolved alias marker is cleared. Clearing every
                // field without a value set also erased the range evidence, so
                // a real `.min().max()` reported as if its source were unknown.
                let Some(alias) = fact.evidence.as_deref().and_then(|e| e.strip_prefix('@')) else {
                    continue;
                };
                match unions.get(alias) {
                    Some(values) => {
                        fact.allowed = Some(values.clone());
                        fact.evidence = Some("a TypeScript string union".to_string());
                    }
                    None => fact.evidence = None,
                }
            }
        }
        types
    }

    fn scan_one(
        &self,
        source: &str,
        types: &mut NodeTypes,
        unions: &mut BTreeMap<String, Vec<String>>,
    ) {
        let lines: Vec<&str> = source.lines().map(strip_comment).collect();
        for (index, line) in lines.iter().enumerate() {
            if let Some(captures) = self.union_alias.captures(line) {
                if let Some(values) = string_union(&captures[2]) {
                    unions.insert(captures[1].to_string(), values);
                    continue;
                }
            }
            if let Some(captures) = self.zod_object.captures(line) {
                let fields = self.zod_fields(&lines, index);
                if !fields.is_empty() {
                    types.shapes.insert(captures[1].to_string(), fields);
                }
                continue;
            }
            if let Some(captures) = self.class_open.captures(line) {
                let fields = self.decorated_fields(&lines, index);
                if !fields.is_empty() {
                    types.shapes.insert(captures[1].to_string(), fields);
                    continue;
                }
            }
            if let Some(captures) = self.interface_open.captures(line) {
                let fields = self.interface_fields(&lines, index);
                if !fields.is_empty() {
                    types
                        .shapes
                        .entry(captures[1].to_string())
                        .or_insert(fields);
                }
            }
            if let Some(captures) = self.function_open.captures(line) {
                let name = captures
                    .get(1)
                    .or_else(|| captures.get(2))
                    .map(|found| found.as_str().to_string());
                if let (Some(name), Some(schema)) = (name, self.parsed_schema(&lines, index)) {
                    types.bodies.insert(name, schema);
                }
            }
        }
    }

    fn zod_fields(&self, lines: &[&str], open: usize) -> BTreeMap<String, FieldFact> {
        let mut fields = BTreeMap::new();
        for line in block(lines, open) {
            let Some(captures) = self.zod_field.captures(&line) else {
                continue;
            };
            let chain = &captures[2];
            let allowed = self
                .zod_enum
                .captures(chain)
                .and_then(|found| string_union(&found[1]));
            let mut low = None;
            let mut high = None;
            for bound in self.zod_bound.captures_iter(chain) {
                let value: f64 = match bound[2].parse() {
                    Ok(value) => value,
                    Err(_) => continue,
                };
                if &bound[1] == "min" {
                    low = Some(value);
                } else {
                    high = Some(value);
                }
            }
            let range = (low.is_some() || high.is_some()).then_some((low, high));
            fields.insert(
                captures[1].to_string(),
                FieldFact {
                    required: !chain.contains(".optional()") && !chain.contains(".nullish()"),
                    evidence: allowed
                        .as_ref()
                        .map(|_| "a zod enum".to_string())
                        .or_else(|| range.map(|_| "a zod min/max".to_string())),
                    allowed,
                    range,
                },
            );
        }
        fields
    }

    /// NestJS DTOs: `@IsIn([...])`, `@Min`, `@Max`, `@IsOptional`.
    fn decorated_fields(&self, lines: &[&str], open: usize) -> BTreeMap<String, FieldFact> {
        let mut fields = BTreeMap::new();
        let mut pending: Vec<(String, String)> = Vec::new();
        for line in block(lines, open) {
            for captures in self.decorator.captures_iter(&line) {
                pending.push((captures[1].to_string(), captures[2].to_string()));
            }
            let Some(captures) = self.class_field.captures(&line) else {
                continue;
            };
            if pending.is_empty() {
                continue;
            }
            let mut fact = FieldFact {
                required: &captures[2] != "?",
                ..FieldFact::default()
            };
            let mut low = None;
            let mut high = None;
            for (name, argument) in pending.drain(..) {
                match name.as_str() {
                    "IsIn" => {
                        let inner = argument
                            .trim()
                            .trim_start_matches('[')
                            .trim_end_matches(']');
                        if let Some(values) = string_union(inner) {
                            fact.allowed = Some(values);
                            fact.evidence = Some("an @IsIn decorator".to_string());
                        }
                    }
                    "Min" => low = argument.trim().parse::<f64>().ok(),
                    "Max" => high = argument.trim().parse::<f64>().ok(),
                    "IsOptional" => fact.required = false,
                    _ => {}
                }
            }
            if low.is_some() || high.is_some() {
                fact.range = Some((low, high));
                fact.evidence
                    .get_or_insert_with(|| "a @Min/@Max decorator".to_string());
            }
            fields.insert(captures[1].to_string(), fact);
        }
        fields
    }

    fn interface_fields(&self, lines: &[&str], open: usize) -> BTreeMap<String, FieldFact> {
        let mut fields = BTreeMap::new();
        for line in block(lines, open) {
            let Some(captures) = self.interface_field.captures(&line) else {
                continue;
            };
            let annotation = captures[3].trim();
            let allowed = string_union(annotation);
            fields.insert(
                captures[1].to_string(),
                FieldFact {
                    required: &captures[2] != "?",
                    evidence: match &allowed {
                        Some(_) => Some("a TypeScript string union".to_string()),
                        // Remember the alias so it can be resolved once every
                        // file has been read.
                        None => Some(format!("@{annotation}")),
                    },
                    allowed,
                    range: None,
                },
            );
        }
        fields
    }

    /// The schema a handler validates its body against.
    fn parsed_schema(&self, lines: &[&str], at: usize) -> Option<String> {
        let end = (at + 40).min(lines.len());
        let body = lines[at..end].join("\n");
        self.parse_call
            .captures(&body)
            .map(|captures| captures[1].to_string())
    }
}

/// The lines of a brace block, same-line or many.
fn block(lines: &[&str], open: usize) -> Vec<String> {
    let end = (open + MAX_BLOCK_LINES).min(lines.len());
    let joined = lines[open..end].join("\n");
    let Some(start) = joined.find('{') else {
        return Vec::new();
    };
    let mut depth = 0i32;
    for (index, character) in joined[start..].char_indices() {
        match character {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return joined[start + 1..start + index]
                        .lines()
                        .map(str::to_string)
                        .collect();
                }
            }
            _ => {}
        }
    }
    Vec::new()
}

/// `'user' | 'sponsor'` or `'user', 'sponsor'` -> the values, or None if any
/// item is not a string literal.
fn string_union(text: &str) -> Option<Vec<String>> {
    let mut values = Vec::new();
    for part in text.split(['|', ',']) {
        let item = part.trim();
        if item.is_empty() {
            continue;
        }
        let unquoted = item.trim_matches(['\'', '"', '`']);
        if unquoted == item {
            return None;
        }
        values.push(unquoted.to_string());
    }
    (values.len() > 1).then_some(values)
}

fn strip_comment(line: &str) -> &str {
    match line.find("//") {
        Some(at) => &line[..at],
        None => line,
    }
}

pub(super) fn scan_types(root: &std::path::Path) -> NodeTypes {
    let scanner = NodeScanner::new(|pattern| Regex::new(pattern).expect("static node pattern"));
    let sources: Vec<String> = super::extract::family_sources(root, super::extract::Family::Node)
        .into_iter()
        .filter_map(|file| std::fs::read_to_string(file).ok())
        .collect();
    scanner.scan(&sources)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan(sources: &[&str]) -> NodeTypes {
        NodeScanner::new(|p| Regex::new(p).expect("pattern"))
            .scan(&sources.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn a_zod_enum_is_a_closed_value_set() {
        let types = scan(&[r#"
const BlockSchema = z.object({
  blocked_type: z.enum(['user', 'sponsor']),
  blocked_id: z.string(),
  rating: z.number().min(-1).max(1),
  note: z.string().optional(),
});
"#]);
        let fields = types.body_fields("BlockSchema").expect("schema is a key");
        assert_eq!(
            fields["blocked_type"].allowed.as_deref(),
            Some(["user".to_string(), "sponsor".to_string()].as_slice())
        );
        assert_eq!(fields["rating"].range, Some((Some(-1.0), Some(1.0))));
        assert!(!fields["note"].required);
        assert!(fields["blocked_id"].required);
        assert_eq!(fields["blocked_id"].allowed, None);
    }

    #[test]
    fn a_handler_that_parses_a_schema_resolves_through_it() {
        let types = scan(&[r#"
const BlockSchema = z.object({ mode: z.enum(['a', 'b']) });
function createBlock(req, res) {
  const body = BlockSchema.parse(req.body);
}
"#]);
        assert!(types.body_fields("createBlock").is_some());
    }

    #[test]
    fn a_typescript_string_union_is_a_closed_value_set() {
        let types = scan(&[r#"
type BlockedType = 'user' | 'sponsor';
interface BlockRequest {
  blocked_type: BlockedType;
  note?: string;
}
"#]);
        let fields = types
            .body_fields("BlockRequest")
            .expect("interface is a key");
        assert_eq!(
            fields["blocked_type"].allowed.as_deref(),
            Some(["user".to_string(), "sponsor".to_string()].as_slice())
        );
        assert!(!fields["note"].required);
        assert_eq!(fields["note"].allowed, None, "an open type stays open");
    }

    #[test]
    fn nest_class_validator_decorators_are_read() {
        let types = scan(&[r#"
class BlockDto {
  @IsIn(['user', 'sponsor'])
  blocked_type: string;

  @Min(-1)
  @Max(1)
  rating: number;

  @IsOptional()
  note: string;
}
"#]);
        let fields = types.body_fields("BlockDto").expect("dto is a key");
        assert_eq!(
            fields["blocked_type"].allowed.as_deref(),
            Some(["user".to_string(), "sponsor".to_string()].as_slice())
        );
        assert_eq!(fields["rating"].range, Some((Some(-1.0), Some(1.0))));
        assert!(!fields["note"].required);
    }

    #[test]
    fn a_union_of_non_literals_abstains() {
        let types = scan(&[r#"
type Thing = SomeType | OtherType;
interface R { thing: Thing; }
"#]);
        let fields = types.body_fields("R").expect("interface is a key");
        assert_eq!(fields["thing"].allowed, None, "not a literal union");
    }
}
