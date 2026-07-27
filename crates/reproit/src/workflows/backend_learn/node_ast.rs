//! JavaScript and TypeScript route and body extraction over the grammar.
//!
//! The pattern reader matched a route registration on one line and a schema
//! declaration on another, and hoped they belonged together. Over a parse the
//! registration's arguments are the arguments: the path, the middleware chain
//! and the handler are distinguishable, so `app.post('/x', validate(S), h)`
//! yields both the handler and the schema it is wrapped in without guessing
//! which token was which.

use super::field_facts::FieldFact;
use super::grammar;
use super::grammar::SourceRead;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use tree_sitter::Node;

const METHODS: [&str; 7] = ["get", "post", "put", "patch", "delete", "head", "options"];
const MAX_FIELDS: usize = 512;

/// A route as first read: the router it hangs off, its local path, the method,
/// the handler, and the schema the registration wraps it in.
type RawRoute = (String, String, &'static str, Option<String>, Option<String>);

/// The grammar for one Node source. TypeScript is a different language to the
/// parser even though it is the same family to the ecosystem, and reading a
/// `.ts` file with the JavaScript grammar produced an error on every type
/// annotation.
fn grammar_for(path: &Path) -> tree_sitter::Language {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some("ts") => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        Some("tsx") => tree_sitter_typescript::LANGUAGE_TSX.into(),
        _ => tree_sitter_javascript::LANGUAGE.into(),
    }
}

pub(super) fn read(root: &Path) -> SourceRead {
    let mut source = SourceRead::default();
    let mut shapes: BTreeMap<String, BTreeMap<String, FieldFact>> = BTreeMap::new();
    let mut ambiguous: BTreeSet<String> = BTreeSet::new();
    let mut raw_routes: Vec<RawRoute> = Vec::new();

    grammar::read_files_with(
        root,
        super::extract::Family::Node,
        |path| Some(grammar_for(path)),
        &mut source,
        |root_node, text, _path| {
            // `app.use('/api', users)` binds a LOCAL name. Keyed globally, one
            // file's mount prefix landed on another file's router.
            // An express-style registration and an HTTP CLIENT call are the
            // same shape: `api.get('/x')` on an axios instance in a React
            // component is not a route the service serves. Only a file that
            // shows evidence of BEING a server is read that way; a decorator
            // controller is unambiguous and is read regardless.
            let server = reads_as_server(text);
            let mut mounts: BTreeMap<String, String> = BTreeMap::new();
            let mut found: Vec<RawRoute> = Vec::new();
            walk(
                root_node,
                text,
                server,
                &mut found,
                &mut shapes,
                &mut ambiguous,
                &mut mounts,
            );
            for (router, path, method, handler, schema) in found {
                let path = match mounts.get(&router) {
                    Some(prefix) => format!("{}{path}", prefix.trim_end_matches('/')),
                    None => path,
                };
                raw_routes.push((String::new(), path, method, handler, schema));
            }
        },
    );
    for name in &ambiguous {
        shapes.remove(name);
    }
    for (_, path, method, handler, schema) in raw_routes {
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

/// Whether `x.get('/path')` in this file registers a route or CALLS one.
///
/// The two are the same shape. Requiring positive server evidence was the
/// obvious gate and the wrong one: `module.exports = (app) => app.get(...)` is
/// an ordinary route file with no such marker, and it would have gone missing.
/// So the test is inverted: a file that pulls in an HTTP CLIENT and shows no
/// sign of building a server is a caller, and its paths are someone else's
/// surface. Everything else is read as before.
fn reads_as_server(text: &str) -> bool {
    const SERVER: [&str; 7] = [
        "express(",
        "Router(",
        "fastify(",
        "Fastify(",
        "new Koa",
        "'express'",
        "\"express\"",
    ];
    const CLIENT: [&str; 6] = [
        "axios",
        "superagent",
        "supertest",
        "node-fetch",
        "'got'",
        "'ky'",
    ];
    SERVER.iter().any(|marker| text.contains(marker))
        || !CLIENT.iter().any(|marker| text.contains(marker))
}

fn walk(
    node: Node,
    text: &str,
    server: bool,
    routes: &mut Vec<RawRoute>,
    shapes: &mut BTreeMap<String, BTreeMap<String, FieldFact>>,
    ambiguous: &mut BTreeSet<String>,
    mounts: &mut BTreeMap<String, String>,
) {
    if server && node.kind() == "call_expression" {
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
    // NestJS states routes as decorators on a controller class, which is a
    // different shape from every express-style registration above: the path is
    // split between a `@Controller('users')` on the class and a `@Get(':id')`
    // on the method, and the decorator is a SIBLING of what it decorates.
    if node.kind() == "class_declaration" {
        nest_controller(node, text, routes);
    }
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
        walk(child, text, server, routes, shapes, ambiguous, mounts);
    }
}

/// A NestJS controller: `@Controller('users')` on the class, an HTTP decorator
/// on each method.
///
/// The class decorator is not a child of the class, it precedes it, so the
/// prefix is read from the enclosing statement's children rather than from the
/// class node.
fn nest_controller(node: Node, text: &str, routes: &mut Vec<RawRoute>) {
    let prefix = match decorator_argument(node, "Controller", text) {
        Some(prefix) => prefix,
        // No `@Controller` means this is an ordinary class, not a route table.
        None => return,
    };
    // `@Controller(RouteKey.Asset)` names a constant this cannot resolve.
    // Emitting the token produced `/RouteKey.Asset/...` -- a path nothing
    // serves -- and buried the real `/assets/...` surface.
    if prefix.contains('.') || prefix.contains('(') {
        return;
    }
    let Some(body) = node.child_by_field_name("body") else {
        return;
    };
    let mut members = Vec::new();
    grammar_children(body, &mut members);
    // Decorators precede the method they belong to, so the walk carries the
    // pending one forward rather than looking inside the method.
    let mut pending: Option<(&'static str, String)> = None;
    for member in members {
        match member.kind() {
            "decorator" => {
                if let Some(found) = nest_verb(member, text) {
                    pending = Some(found);
                }
            }
            "method_definition" => {
                let Some((method, suffix)) = pending.take() else {
                    continue;
                };
                let handler = member
                    .child_by_field_name("name")
                    .map(|name| grammar_text(name, text).to_string());
                routes.push((
                    String::new(),
                    join_nest(&prefix, &suffix),
                    method,
                    handler,
                    None,
                ));
            }
            _ => {}
        }
    }
}

/// `@Get(':id')` -> the verb and its path suffix.
fn nest_verb(decorator: Node, text: &str) -> Option<(&'static str, String)> {
    let call = decorator.named_child(0)?;
    let (name, argument) = match call.kind() {
        "call_expression" => {
            let name = call.child_by_field_name("function")?;
            let mut args = Vec::new();
            if let Some(list) = call.child_by_field_name("arguments") {
                grammar_children(list, &mut args);
            }
            (
                grammar_text(name, text).to_string(),
                args.first()
                    .map(|arg| {
                        grammar_text(*arg, text)
                            .trim_matches(['"', '\'', '`'])
                            .to_string()
                    })
                    .unwrap_or_default(),
            )
        }
        // A bare `@Get` with no parentheses maps the controller path itself.
        "identifier" => (grammar_text(call, text).to_string(), String::new()),
        _ => return None,
    };
    let verb = name.to_ascii_lowercase();
    METHODS
        .into_iter()
        .find(|known| *known == verb)
        .map(|method| (method, argument))
}

/// The single string argument of a named decorator preceding `node`.
fn decorator_argument(node: Node, name: &str, text: &str) -> Option<String> {
    let parent = node.parent()?;
    let mut siblings = Vec::new();
    grammar_children(parent, &mut siblings);
    for sibling in siblings {
        if sibling.kind() != "decorator" {
            continue;
        }
        let raw = grammar_text(sibling, text);
        if !raw.trim_start_matches('@').starts_with(name) {
            continue;
        }
        let Some(call) = sibling.named_child(0) else {
            continue;
        };
        if call.kind() != "call_expression" {
            return Some(String::new());
        }
        let mut args = Vec::new();
        if let Some(list) = call.child_by_field_name("arguments") {
            grammar_children(list, &mut args);
        }
        return Some(
            args.first()
                .map(|arg| {
                    grammar_text(*arg, text)
                        .trim_matches(['"', '\'', '`'])
                        .to_string()
                })
                .unwrap_or_default(),
        );
    }
    None
}

fn join_nest(prefix: &str, suffix: &str) -> String {
    let base = prefix.trim_matches('/');
    let suffix = suffix.trim_matches('/');
    match (base.is_empty(), suffix.is_empty()) {
        (true, true) => "/".to_string(),
        (true, false) => format!("/{suffix}"),
        (false, true) => format!("/{base}"),
        (false, false) => format!("/{base}/{suffix}"),
    }
}

fn grammar_children<'a>(node: Node<'a>, into: &mut Vec<Node<'a>>) {
    super::grammar::children(node, into);
}

fn grammar_text<'a>(node: Node, source: &'a str) -> &'a str {
    super::grammar::text(node, source)
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

    fn read_source(case: &str, files: &[(&str, &str)]) -> SourceRead {
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
    fn a_zod_default_makes_a_field_optional() {
        let source = read_source(
            "zod_default",
            &[(
                "server.js",
                "const S = z.object({ a: z.string(), b: z.string().default('x'), \
                 c: z.number().catch(0) });\napp.post('/x', S);\n",
            )],
        );
        let fields = source.bodies.get("S").expect("resolved");
        assert!(fields["a"].required);
        assert!(!fields["b"].required, ".default() opts out");
        assert!(!fields["c"].required, ".catch() opts out");
    }

    #[test]
    fn typescript_reads_through_its_own_grammar() {
        // `.ts` was in the extension list but parsed with the JavaScript
        // grammar, so every type annotation was an error and a whole
        // TypeScript service came back as zero routes -- indistinguishable
        // from a service with no routes at all.
        let source = read_source(
            "typescript",
            &[(
                "server.ts",
                "import express, { Request, Response, Router } from 'express';\n\
                 interface Item { name: string }\n\
                 const app = express();\n\
                 const users: Router = express.Router();\n\
                 users.get('/list', (req: Request, res: Response): void => { res.json([]); });\n\
                 app.use('/api', users);\n\
                 app.get('/status', (req: Request, res: Response): void => { res.send('ok'); });\n",
            )],
        );
        assert_eq!(source.files_unreadable, 0, "TypeScript must parse cleanly");
        let paths: Vec<&String> = source.routes.iter().map(|(path, _, _)| path).collect();
        assert!(paths.contains(&&"/status".to_string()), "{paths:?}");
        assert!(
            paths.contains(&&"/api/list".to_string()),
            "a mounted TS router keeps its prefix: {paths:?}"
        );
    }

    #[test]
    fn a_nestjs_controller_composes_its_decorator_paths() {
        // NestJS is TypeScript but shares nothing with an express registration:
        // the path is split between a class decorator and a method decorator,
        // and the decorator PRECEDES what it decorates rather than nesting in it.
        let source = read_source(
            "nestjs",
            &[(
                "users.controller.ts",
                "import { Controller, Get, Post, Body, Param } from '@nestjs/common';\n\
                 @Controller('users')\n\
                 export class UsersController {\n\
                 \x20 @Get()\n  findAll(): string { return 'all'; }\n\
                 \x20 @Get(':id')\n  findOne(@Param('id') id: string): string { return id; }\n\
                 \x20 @Post()\n  create(@Body() dto: CreateUserDto): string { return 'made'; }\n\
                 }\n\
                 @Controller()\n\
                 export class HealthController {\n\
                 \x20 @Get('health')\n  health(): string { return 'ok'; }\n\
                 }\n",
            )],
        );
        let paths: Vec<&String> = source.routes.iter().map(|(path, _, _)| path).collect();
        assert!(paths.contains(&&"/users".to_string()), "{paths:?}");
        assert!(paths.contains(&&"/users/:id".to_string()), "{paths:?}");
        assert!(
            paths.contains(&&"/health".to_string()),
            "a bare @Controller() has no prefix: {paths:?}"
        );
        let post = source
            .routes
            .iter()
            .find(|(path, method, _)| path == "/users" && *method == "post");
        assert_eq!(
            post.and_then(|(_, _, handler)| handler.clone()),
            Some("create".to_string())
        );
    }

    #[test]
    fn an_ordinary_typescript_class_is_not_a_route_table() {
        let source = read_source(
            "plainclass",
            &[(
                "service.ts",
                "export class UserService {\n  findAll(): string { return 'all'; }\n}\n",
            )],
        );
        assert!(source.routes.is_empty(), "{:?}", source.routes);
    }

    #[test]
    fn a_test_source_is_not_the_served_surface() {
        // `request(app).get('/1/abc')` is a URL the test DRIVES. Reading them
        // made 144 of 162 reported NestJS paths fictional.
        let source = read_source(
            "testfiles",
            &[
                (
                    "server.js",
                    "const app = require('express')();\napp.get('/real', h);\n",
                ),
                (
                    "server.spec.js",
                    "const request = require('supertest');\nrequest(app).get('/1/abc');\n",
                ),
            ],
        );
        let paths: Vec<&String> = source.routes.iter().map(|(path, _, _)| path).collect();
        assert_eq!(paths, vec![&"/real".to_string()], "{paths:?}");
    }

    #[test]
    fn an_http_client_call_is_not_a_route() {
        // `api.get('/x')` on an axios instance is someone else's surface.
        let source = read_source(
            "client",
            &[
                (
                    "server.js",
                    "const app = require('express')();\napp.get('/real', h);\n",
                ),
                (
                    "client.js",
                    "import axios from 'axios';\nconst api = axios.create({});\n\
                     api.get('/api/frontend-only');\n",
                ),
            ],
        );
        let paths: Vec<&String> = source.routes.iter().map(|(path, _, _)| path).collect();
        assert_eq!(paths, vec![&"/real".to_string()], "{paths:?}");
    }

    #[test]
    fn a_non_literal_controller_argument_abstains() {
        // `@Controller(RouteKey.Asset)` emitted `/RouteKey.Asset`, burying the
        // real `/assets` surface behind a path nothing serves.
        let source = read_source(
            "enumprefix",
            &[(
                "a.controller.ts",
                "import { Controller, Get } from '@nestjs/common';\n\
                 @Controller(RouteKey.Asset)\nexport class A { @Get(':id') one(): string { return ''; } }\n",
            )],
        );
        assert!(
            source.routes.is_empty(),
            "an unresolvable prefix must abstain: {:?}",
            source.routes
        );
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
