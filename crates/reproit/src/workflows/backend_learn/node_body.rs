//! Request-body field names read from an INLINE express-style handler.
//!
//! A zod schema names its fields declaratively and `node_ast` reads those.
//! Most plain Express code has no schema at all; the field names live in the
//! handler itself, as `const { name, price } = req.body` or `req.body.name`.
//! Both are exact statements of what the handler reaches for, so both are
//! honest sources for a synthesized probe body. Nothing here claims a type
//! or required-ness: a name read from a destructure is only a name.

use super::field_facts::FieldFact;
use super::grammar;
use std::collections::BTreeMap;
use tree_sitter::Node;

/// Bound the fields one handler may contribute, matching the grammar cap.
const MAX_FIELDS: usize = grammar::MAX_FIELDS;

/// The body fields an inline function handler reads, or None when the node is
/// not a function or reads none. Only the handler's OWN request parameter is
/// trusted: `other.body` in a nested closure is not this route's body.
pub(super) fn inline_fields(node: Node, text: &str) -> Option<BTreeMap<String, FieldFact>> {
    if !matches!(
        node.kind(),
        "arrow_function" | "function_expression" | "function_declaration" | "function"
    ) {
        return None;
    }
    let request = first_parameter_name(node, text)?;
    let body_expression = format!("{request}.body");
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
                        fields.entry(name).or_insert_with(field_fact);
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
                    fields.entry(name).or_insert_with(field_fact);
                }
            }
            _ => {}
        }
    });
    (!fields.is_empty()).then_some(fields)
}

fn field_fact() -> FieldFact {
    FieldFact {
        evidence: Some("read from the request body in the handler".to_string()),
        ..FieldFact::default()
    }
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

    fn fields_of(source: &str) -> Option<BTreeMap<String, FieldFact>> {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_javascript::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        assert!(!tree.root_node().has_error(), "fixture must parse");
        let mut found = None;
        grammar::walk(tree.root_node(), &mut |node| {
            if found.is_none() {
                found = inline_fields(node, source);
            }
        });
        found
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
}
