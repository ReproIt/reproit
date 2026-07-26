//! JavaScript and TypeScript route and body extraction over the grammar.
//!
//! The pattern reader matched a route registration on one line and a schema
//! declaration on another, and hoped they belonged together. Over a parse the
//! registration's arguments are the arguments: the path, the middleware chain
//! and the handler are distinguishable, so `app.post('/x', validate(S), h)`
//! yields both the handler and the schema it is wrapped in without guessing
//! which token was which.

use super::field_facts::FieldFact;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use tree_sitter::{Node, Parser};

const METHODS: [&str; 7] = ["get", "post", "put", "patch", "delete", "head", "options"];
const MAX_FIELDS: usize = 512;

/// A route as first read: the router it hangs off, its local path, the method,
/// the handler, and the schema the registration wraps it in.
type RawRoute = (String, String, &'static str, Option<String>, Option<String>);

#[derive(Debug, Default)]
pub(super) struct NodeSource {
    pub(super) routes: Vec<(String, &'static str, Option<String>)>,
    pub(super) bodies: BTreeMap<String, BTreeMap<String, FieldFact>>,
    pub(super) files_parsed: usize,
    pub(super) files_unreadable: usize,
}

pub(super) fn read(root: &Path) -> NodeSource {
    let mut source = NodeSource::default();
    let mut parser = Parser::new();
    if parser
        .set_language(&tree_sitter_javascript::LANGUAGE.into())
        .is_err()
    {
        return source;
    }
    let mut shapes: BTreeMap<String, BTreeMap<String, FieldFact>> = BTreeMap::new();
    let mut ambiguous: BTreeSet<String> = BTreeSet::new();
    let mut mounts: BTreeMap<String, String> = BTreeMap::new();
    let mut raw_routes: Vec<RawRoute> = Vec::new();

    for file in super::extract::family_sources(root, super::extract::Family::Node) {
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        let Some(tree) = parser.parse(&text, None) else {
            source.files_unreadable += 1;
            continue;
        };
        if tree.root_node().has_error() {
            source.files_unreadable += 1;
            continue;
        }
        source.files_parsed += 1;
        walk(
            tree.root_node(),
            &text,
            &mut raw_routes,
            &mut shapes,
            &mut ambiguous,
            &mut mounts,
        );
    }
    for name in &ambiguous {
        shapes.remove(name);
    }
    for (router, path, method, handler, schema) in raw_routes {
        // `app.use('/api', router)` mounts a router under a prefix.
        let path = match mounts.get(&router) {
            Some(prefix) => format!("{}{path}", prefix.trim_end_matches('/')),
            None => path,
        };
        source.routes.push((path, method, handler.clone()));
        // Resolve the body under the handler's name, from whichever of the two
        // actually declared a shape.
        if let Some(handler) = handler {
            let fields = shapes
                .get(&handler)
                .or_else(|| schema.as_ref().and_then(|name| shapes.get(name)));
            if let Some(fields) = fields {
                source.bodies.insert(handler, fields.clone());
            }
        }
    }
    source
}

fn walk(
    node: Node,
    text: &str,
    routes: &mut Vec<RawRoute>,
    shapes: &mut BTreeMap<String, BTreeMap<String, FieldFact>>,
    ambiguous: &mut BTreeSet<String>,
    mounts: &mut BTreeMap<String, String>,
) {
    if node.kind() == "call_expression" {
        if let Some(member) = node.child_by_field_name("function") {
            let object = member
                .child_by_field_name("object")
                .and_then(|n| n.utf8_text(text.as_bytes()).ok())
                .unwrap_or_default()
                .to_string();
            let property = member
                .child_by_field_name("property")
                .and_then(|n| n.utf8_text(text.as_bytes()).ok())
                .unwrap_or_default()
                .to_string();
            if let Some(arguments) = node.child_by_field_name("arguments") {
                let mut cursor = arguments.walk();
                let args: Vec<Node> = arguments
                    .children(&mut cursor)
                    .filter(|child| child.is_named())
                    .collect();
                let first = args.first().and_then(|node| string_value(*node, text));
                // fastify's object form: `.route({ method: 'PUT', url: '/x' })`.
                if property == "route" {
                    if let Some((path, method, handler)) = fastify_route(&args, text) {
                        routes.push((object.clone(), path, method, handler, None));
                    }
                }
                if let Some(path) = first.filter(|path| path.starts_with('/')) {
                    if property == "use" {
                        if let Some(name) = args.get(1).and_then(|node| identifier(*node, text)) {
                            mounts.insert(name, path);
                        }
                    } else if let Some(method) = METHODS.iter().find(|method| **method == property)
                    {
                        // The last named argument is conventionally the
                        // handler; a `validate(Schema)` earlier in the chain
                        // names the schema. Both are recorded, because the body
                        // has to be found under the HANDLER even when the fields
                        // were declared under the schema.
                        let handler = args
                            .iter()
                            .skip(1)
                            .rev()
                            .find_map(|node| identifier(*node, text));
                        let schema = args
                            .iter()
                            .skip(1)
                            .find_map(|node| wrapped_argument(*node, text));
                        routes.push((object, path, *method, handler, schema));
                    }
                }
            }
        }
    }
    // `const Schema = z.object({ ... })`
    if node.kind() == "variable_declarator" {
        if let (Some(name), Some(value)) = (
            node.child_by_field_name("name")
                .and_then(|n| n.utf8_text(text.as_bytes()).ok())
                .map(str::to_string),
            node.child_by_field_name("value"),
        ) {
            if let Some(fields) = zod_object(value, text) {
                match shapes.get(&name) {
                    Some(existing) if *existing != fields => {
                        ambiguous.insert(name);
                    }
                    Some(_) => {}
                    None => {
                        shapes.insert(name, fields);
                    }
                }
            }
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk(child, text, routes, shapes, ambiguous, mounts);
    }
}

/// The fields of a `z.object({ ... })`, or None if this is not one.
fn zod_object(node: Node, text: &str) -> Option<BTreeMap<String, FieldFact>> {
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
        required: !chain.contains(".optional()") && !chain.contains(".nullish()"),
        evidence: match (&allowed, &range) {
            (Some(_), _) => Some("a zod enum".to_string()),
            (_, Some(_)) => Some("a zod min/max".to_string()),
            _ => None,
        },
        allowed,
        range,
    }
}

/// `validate(BlockSchema)` -> `BlockSchema`.
fn wrapped_argument(node: Node, text: &str) -> Option<String> {
    if node.kind() != "call_expression" {
        return None;
    }
    let arguments = node.child_by_field_name("arguments")?;
    let mut cursor = arguments.walk();
    let found = arguments
        .children(&mut cursor)
        .find_map(|child| identifier(child, text));
    found
}

fn identifier(node: Node, text: &str) -> Option<String> {
    (node.kind() == "identifier")
        .then(|| node.utf8_text(text.as_bytes()).ok())
        .flatten()
        .map(str::to_string)
}

fn string_value(node: Node, text: &str) -> Option<String> {
    matches!(node.kind(), "string" | "template_string")
        .then(|| node.utf8_text(text.as_bytes()).ok())
        .flatten()
        .map(|raw| raw.trim_matches(['"', '\'', '`']).to_string())
}

fn literal_values(inner: &str) -> Option<Vec<String>> {
    let mut values = Vec::new();
    for part in inner.split(',') {
        let item = part.trim();
        if item.is_empty() {
            continue;
        }
        let unquoted = item.trim_matches(['"', '\'', '`']);
        if unquoted == item {
            return None;
        }
        values.push(unquoted.to_string());
    }
    (values.len() > 1).then_some(values)
}

/// fastify's `route({ method: 'PUT', url: '/users/:id', handler: h })`, where
/// the verb and the path are object properties rather than call arguments.
fn fastify_route(args: &[Node], text: &str) -> Option<(String, &'static str, Option<String>)> {
    let object = args.iter().find(|node| node.kind() == "object")?;
    let mut cursor = object.walk();
    let mut url = None;
    let mut verb = None;
    let mut handler = None;
    for pair in object.children(&mut cursor) {
        if pair.kind() != "pair" {
            continue;
        }
        let key = pair
            .child_by_field_name("key")
            .and_then(|node| node.utf8_text(text.as_bytes()).ok())
            .map(|key| key.trim_matches(['"', '\'', '`']).to_string())
            .unwrap_or_default();
        let Some(value) = pair.child_by_field_name("value") else {
            continue;
        };
        match key.as_str() {
            "url" | "path" => url = string_value(value, text),
            "method" => {
                verb = string_value(value, text).map(|verb| verb.to_lowercase());
            }
            "handler" => handler = identifier(value, text),
            _ => {}
        }
    }
    let url = url?;
    let verb = verb?;
    let method = METHODS.iter().find(|method| **method == verb)?;
    Some((url, *method, handler))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_source(case: &str, files: &[(&str, &str)]) -> NodeSource {
        let root =
            std::env::temp_dir().join(format!("reproit-jsast-{}-{case}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("root");
        for (name, body) in files {
            std::fs::write(root.join(name), body).expect("write");
        }
        let source = read(&root);
        let _ = std::fs::remove_dir_all(&root);
        source
    }

    #[test]
    fn a_route_and_the_schema_it_validates_with_are_one_registration() {
        let source = read_source(
            "zod",
            &[(
                "server.js",
                "const BlockSchema = z.object({\n  blocked_type: z.enum(['user','sponsor']),\n\
                 \x20 rating: z.number().min(-1).max(1),\n  note: z.string().optional(),\n});\n\
                 app.post('/v1/blocks', validate(BlockSchema), createBlock);\n",
            )],
        );
        assert_eq!(
            source.routes,
            vec![("/v1/blocks".to_string(), "post", Some("createBlock".into()))]
        );
        // The body is found under the HANDLER even though the fields were
        // declared under the schema the route wraps it in.
        let fields = source.bodies.get("createBlock").expect("resolved");
        assert_eq!(
            fields["blocked_type"].allowed.as_deref(),
            Some(["user".to_string(), "sponsor".to_string()].as_slice())
        );
        assert_eq!(fields["rating"].range, Some((Some(-1.0), Some(1.0))));
        assert!(!fields["note"].required);
    }

    #[test]
    fn a_schema_named_directly_by_the_route_resolves_its_fields() {
        let source = read_source(
            "direct",
            &[(
                "server.js",
                "const S = z.object({ mode: z.enum(['a','b']), n: z.number().min(1).max(5) });\n\
                 app.post('/x', S);\n",
            )],
        );
        let fields = source.bodies.get("S").expect("resolved");
        assert_eq!(
            fields["mode"].allowed.as_deref(),
            Some(["a".to_string(), "b".to_string()].as_slice())
        );
        assert_eq!(fields["n"].range, Some((Some(1.0), Some(5.0))));
    }

    #[test]
    fn a_router_mounted_with_use_carries_its_prefix() {
        let source = read_source(
            "mount",
            &[(
                "server.js",
                "users.get('/list', listUsers);\napp.use('/api', users);\n",
            )],
        );
        assert_eq!(
            source.routes,
            vec![("/api/list".to_string(), "get", Some("listUsers".into()))]
        );
    }

    #[test]
    fn an_optional_zod_field_is_not_required() {
        let source = read_source(
            "optional",
            &[(
                "server.js",
                "const S = z.object({ a: z.string(), b: z.string().optional() });\n\
                 app.post('/x', S);\n",
            )],
        );
        let fields = source.bodies.get("S").expect("resolved");
        assert!(fields["a"].required);
        assert!(!fields["b"].required);
    }

    #[test]
    fn a_file_that_does_not_parse_is_counted() {
        let source = read_source(
            "broken",
            &[("ok.js", "const x = 1;\n"), ("bad.js", "function f( {\n")],
        );
        assert_eq!(source.files_parsed, 1);
        assert_eq!(source.files_unreadable, 1);
    }

    #[test]
    fn a_non_route_call_with_a_string_is_not_a_route() {
        let source = read_source(
            "notaroute",
            &[("server.js", "console.log('/not/a/route');\n")],
        );
        assert!(source.routes.is_empty(), "{:?}", source.routes);
    }
}
