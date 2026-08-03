//! What a Node request is SHAPED like, read from source.
//!
//! Most plain Express code has no schema at all; the field names live in the
//! handler itself, as `const { name, price } = req.body` or `req.body.name`.
//! Both are exact statements of what the handler reaches for, so both are
//! honest sources for a synthesized probe body. Nothing here claims a type
//! or required-ness: a name read from a destructure is only a name.
//!
//! A `z.object({...})` states the same field set declaratively, and it is read
//! here too: one file owns what a Node request is SHAPED like, whether the
//! source says so in a schema or in the handler's own destructures.
//!
//! `req.query` is read the same way and by the same rules. Until it was, a
//! `/search?q=` route derived as a bare path: the parameter the handler
//! branches on had no name in the draft, so no oracle and no generated request
//! could speak about it at all.

use super::field_facts::FieldFact;
use super::grammar;
use std::collections::BTreeMap;
use tree_sitter::Node;

/// Bound the fields one handler may contribute, matching the grammar cap.
const MAX_FIELDS: usize = grammar::MAX_FIELDS;

/// What one inline handler states about its request. Either map may be empty:
/// a GET route names query parameters and no body, a POST usually the reverse.
#[derive(Debug, Default)]
pub(super) struct InlineRequest {
    pub(super) body: BTreeMap<String, FieldFact>,
    pub(super) query: BTreeMap<String, FieldFact>,
}

/// The body and query fields an inline function handler reads, or None when
/// the node is not a function or reads neither. Only the handler's OWN request
/// parameter is trusted: `other.body` in a nested closure is not this route's.
pub(super) fn inline_request(node: Node, text: &str) -> Option<InlineRequest> {
    if !matches!(
        node.kind(),
        "arrow_function" | "function_expression" | "function_declaration" | "function"
    ) {
        return None;
    }
    let request = first_parameter_name(node, text)?;
    let found = InlineRequest {
        body: fields_of(node, text, &request, "body"),
        query: fields_of(node, text, &request, "query"),
    };
    (!found.body.is_empty() || !found.query.is_empty()).then_some(found)
}

/// The names one `request.<part>` is destructured into or read through.
fn fields_of(
    node: Node,
    text: &str,
    request: &str,
    part: &'static str,
) -> BTreeMap<String, FieldFact> {
    let body_expression = format!("{request}.{part}");
    let mut fields = BTreeMap::new();
    grammar::walk(node, &mut |child| {
        if fields.len() >= MAX_FIELDS {
            return;
        }
        match child.kind() {
            // `const { name, price } = req.body`
            "variable_declarator" => {
                let destructures_body = child
                    .child_by_field_name("value")
                    .is_some_and(|value| grammar::text(value, text) == body_expression);
                if !destructures_body {
                    return;
                }
                let Some(pattern) = child
                    .child_by_field_name("name")
                    .filter(|name| name.kind() == "object_pattern")
                else {
                    return;
                };
                let mut members = Vec::new();
                grammar::children(pattern, &mut members);
                for member in members {
                    let name = match member.kind() {
                        "shorthand_property_identifier_pattern" => {
                            Some(grammar::text(member, text).to_string())
                        }
                        // `{ name: renamed }` and `{ name = fallback }`: the
                        // BODY field is the key, not the local binding.
                        "pair_pattern" | "object_assignment_pattern" => member
                            .named_child(0)
                            .map(|key| grammar::text(key, text).to_string()),
                        _ => None,
                    };
                    if let Some(name) = name.filter(|name| is_identifier(name)) {
                        fields.entry(name).or_insert_with(|| field_fact(part));
                    }
                }
            }
            // `req.body.name`
            "member_expression" => {
                let reads_body = child
                    .child_by_field_name("object")
                    .is_some_and(|object| grammar::text(object, text) == body_expression);
                if !reads_body {
                    return;
                }
                let Some(name) = child
                    .child_by_field_name("property")
                    .map(|property| grammar::text(property, text).to_string())
                else {
                    return;
                };
                if is_identifier(&name) {
                    fields.entry(name).or_insert_with(|| field_fact(part));
                }
            }
            _ => {}
        }
    });
    fields
}

fn field_fact(part: &'static str) -> FieldFact {
    let where_read = match part {
        "query" => "query string",
        _ => "body",
    };
    FieldFact {
        evidence: Some(format!("read from the request {where_read} in the handler")),
        ..FieldFact::default()
    }
}

/// The fields of a `z.object({ ... })`, or None if this is not one.
pub(super) fn zod_object(node: Node, text: &str) -> Option<BTreeMap<String, FieldFact>> {
    let raw = node.utf8_text(text.as_bytes()).ok()?;
    if !raw.contains("z.object") && !raw.contains("z\n") {
        return None;
    }
    let arguments = node.child_by_field_name("arguments")?;
    let mut cursor = arguments.walk();
    let object = arguments
        .children(&mut cursor)
        .find(|child| child.kind() == "object")?;
    let mut fields = BTreeMap::new();
    let mut pairs = object.walk();
    for pair in object.children(&mut pairs).take(MAX_FIELDS) {
        if pair.kind() != "pair" {
            continue;
        }
        let Some(name) = pair
            .child_by_field_name("key")
            .and_then(|node| node.utf8_text(text.as_bytes()).ok())
            .map(|name| name.trim_matches(['"', '\'', '`']).to_string())
        else {
            continue;
        };
        let chain = pair
            .child_by_field_name("value")
            .and_then(|node| node.utf8_text(text.as_bytes()).ok())
            .unwrap_or_default();
        fields.insert(name, zod_fact(chain));
    }
    (!fields.is_empty()).then_some(fields)
}

fn zod_fact(chain: &str) -> FieldFact {
    let allowed = chain
        .split_once(".enum(")
        .and_then(|(_, rest)| rest.split_once(']'))
        .and_then(|(inner, _)| literal_values(inner.trim_start_matches('[')));
    let bound = |key: &str| -> Option<f64> {
        let compact: String = chain.chars().filter(|c| !c.is_whitespace()).collect();
        let value = compact.split(key).nth(1)?;
        let literal: String = value
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '-' || *c == '.')
            .collect();
        literal.parse().ok()
    };
    let low = bound(".min(");
    let high = bound(".max(");
    let range = (low.is_some() || high.is_some()).then_some((low, high));
    FieldFact {
        // `.default(x)` and `.catch(x)` make the INPUT optional just as surely
        // as `.optional()`: omitting the field yields the fallback rather than
        // a rejection, so calling it required states a rejection that does not
        // happen. Same shape as Rust's `#[serde(default)]`.
        required: !chain.contains(".optional()")
            && !chain.contains(".nullish()")
            && !chain.contains(".default(")
            && !chain.contains(".catch("),
        evidence: match (&allowed, &range) {
            (Some(_), _) => Some("a zod enum".to_string()),
            (_, Some(_)) => Some("a zod min/max".to_string()),
            _ => None,
        },
        allowed,
        range,
    }
}

fn literal_values(inner: &str) -> Option<Vec<String>> {
    // JS strings add the backtick; a bare number in an enum list is not idiomatic.
    super::field_facts::literal_values(inner, &['"', '\'', '`'], false)
}

/// The name of the function's first parameter (`req` in `(req, res) => ...`),
/// including a bare-identifier arrow parameter.
fn first_parameter_name(node: Node, text: &str) -> Option<String> {
    if let Some(parameter) = node
        .child_by_field_name("parameter")
        .filter(|parameter| parameter.kind() == "identifier")
    {
        return Some(grammar::text(parameter, text).to_string());
    }
    let parameters = node.child_by_field_name("parameters")?;
    let mut members = Vec::new();
    grammar::children(parameters, &mut members);
    let first = members.first()?;
    let identifier = match first.kind() {
        "identifier" => *first,
        // A typed TS parameter: `(req: Request, ...)`.
        "required_parameter" | "optional_parameter" => first
            .child_by_field_name("pattern")
            .filter(|pattern| pattern.kind() == "identifier")?,
        _ => return None,
    };
    Some(grammar::text(identifier, text).to_string())
}

fn is_identifier(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_')
        && !name.starts_with(|character: char| character.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tree_sitter::Parser;

    fn request_of(source: &str) -> Option<InlineRequest> {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_javascript::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        assert!(!tree.root_node().has_error(), "fixture must parse");
        let mut found = None;
        grammar::walk(tree.root_node(), &mut |node| {
            if found.is_none() {
                found = inline_request(node, source);
            }
        });
        found
    }

    fn fields_of(source: &str) -> Option<BTreeMap<String, FieldFact>> {
        request_of(source).map(|request| request.body)
    }

    #[test]
    fn a_destructured_request_body_names_its_fields() {
        let fields = fields_of(
            "app.post('/items', (req, res) => {\n\
             \x20 const { name, price } = req.body;\n\
             \x20 res.json({ name: name.trim(), price });\n\
             });\n",
        )
        .expect("fields read");
        assert_eq!(
            fields.keys().collect::<Vec<_>>(),
            vec!["name", "price"],
            "the destructured names are the body fields"
        );
        assert!(
            !fields["name"].required,
            "a destructure states no requiredness"
        );
    }

    #[test]
    fn direct_member_reads_and_renamed_destructures_resolve_the_body_key() {
        let fields = fields_of(
            "app.post('/x', (request) => {\n\
             \x20 const { kind: k, note = '' } = request.body;\n\
             \x20 return request.body.count + k + note;\n\
             });\n",
        )
        .expect("fields read");
        assert_eq!(
            fields.keys().collect::<Vec<_>>(),
            vec!["count", "kind", "note"]
        );
    }

    #[test]
    fn a_foreign_object_or_a_bodyless_handler_yields_nothing() {
        assert!(
            fields_of("app.post('/x', (req, res) => { const { a } = other.body; });\n").is_none(),
            "another object's `.body` is not this route's request body"
        );
        assert!(fields_of("app.get('/x', (req, res) => { res.end('ok'); });\n").is_none());
    }

    #[test]
    fn a_query_parameter_is_named_by_the_same_two_shapes_as_a_body_field() {
        let destructured = request_of(
            "app.get('/search', (req, res) => {\n\
             \x20 const { q, limit } = req.query;\n\
             \x20 res.json({ q, limit });\n\
             });\n",
        )
        .expect("query read");
        assert_eq!(
            destructured.query.keys().collect::<Vec<_>>(),
            vec!["limit", "q"]
        );
        assert!(destructured.body.is_empty(), "a GET states no body");
        assert_eq!(
            destructured.query["q"].evidence.as_deref(),
            Some("read from the request query string in the handler"),
            "the evidence names where the name was read, not just that it was"
        );

        let member =
            request_of("app.get('/search', (req, res) => { res.json(find(req.query.q)); });\n")
                .expect("query read");
        assert_eq!(member.query.keys().collect::<Vec<_>>(), vec!["q"]);
    }

    #[test]
    fn a_handler_that_reads_both_states_both() {
        let found = request_of(
            "app.post('/items', (req, res) => {\n\
             \x20 const { name } = req.body;\n\
             \x20 if (req.query.dryRun) return res.end();\n\
             \x20 res.json({ name });\n\
             });\n",
        )
        .expect("fields read");
        assert_eq!(found.body.keys().collect::<Vec<_>>(), vec!["name"]);
        assert_eq!(found.query.keys().collect::<Vec<_>>(), vec!["dryRun"]);
    }
}
