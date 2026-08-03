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
use super::route_path::join_segments as join_nest;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use tree_sitter::Node;

const METHODS: [&str; 7] = ["get", "post", "put", "patch", "delete", "head", "options"];

/// A route as first read: the router it hangs off, its local path, the method,
/// the handler, and the schema the registration wraps it in.
type RawRoute = (String, String, &'static str, Option<String>, Option<String>);

#[derive(Debug)]
struct ModuleRead {
    path: PathBuf,
    routes: Vec<RawRoute>,
    mounts: BTreeMap<String, String>,
    imports: BTreeMap<String, String>,
    routers: BTreeSet<String>,
}

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
    // Query parameters are read only from an inline handler, whose synthesized
    // name already carries its method and path, so one flat map across the
    // walk cannot collide the way a bare function name can.
    let mut queries: BTreeMap<String, BTreeMap<String, FieldFact>> = BTreeMap::new();
    let mut ambiguous: BTreeSet<String> = BTreeSet::new();
    let mut modules = Vec::new();

    grammar::read_files_with(
        root,
        super::extract::Family::Node,
        |path| Some(grammar_for(path)),
        &mut source,
        |root_node, text, path| {
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
            let mut imports = BTreeMap::new();
            let mut routers = BTreeSet::new();
            {
                let mut state = WalkState {
                    routes: &mut found,
                    shapes: &mut shapes,
                    queries: &mut queries,
                    ambiguous: &mut ambiguous,
                    mounts: &mut mounts,
                    imports: &mut imports,
                    routers: &mut routers,
                };
                walk(root_node, text, server, &mut state);
            }
            modules.push(ModuleRead {
                path: path.to_path_buf(),
                routes: found,
                mounts,
                imports,
                routers,
            });
        },
    );
    let external_mounts = resolve_external_mounts(&modules);
    for name in &ambiguous {
        shapes.remove(name);
    }
    for module in modules {
        for (router, path, method, handler, schema) in module.routes {
            let prefixes: Vec<Option<&str>> = match module.mounts.get(&router) {
                Some(prefix) => vec![Some(prefix)],
                None if module.routers.contains(&router) => external_mounts
                    .get(&module.path)
                    .map(|prefixes| {
                        prefixes
                            .iter()
                            .map(|prefix| Some(prefix.as_str()))
                            .collect()
                    })
                    .unwrap_or_default(),
                None => vec![None],
            };
            for prefix in prefixes {
                let path = prefix
                    .map(|prefix| format!("{}{path}", prefix.trim_end_matches('/')))
                    .unwrap_or_else(|| path.clone());
                source.routes.push((path, method, handler.clone()));
                // Resolve the body under the handler's name, from whichever of
                // the two actually declared a shape.
                if let Some(handler) = &handler {
                    let fields = shapes
                        .get(handler)
                        .or_else(|| schema.as_ref().and_then(|name| shapes.get(name)));
                    if let Some(fields) = fields {
                        source.bodies.insert(handler.clone(), fields.clone());
                    }
                    if let Some(fields) = queries.get(handler) {
                        source.queries.insert(handler.clone(), fields.clone());
                    }
                }
            }
        }
    }
    source
}

fn resolve_external_mounts(modules: &[ModuleRead]) -> BTreeMap<PathBuf, Vec<String>> {
    let known: BTreeSet<PathBuf> = modules.iter().map(|module| module.path.clone()).collect();
    let mut mounted: BTreeMap<PathBuf, Vec<String>> = BTreeMap::new();
    for module in modules {
        for (binding, prefix) in &module.mounts {
            let Some(specifier) = module.imports.get(binding) else {
                continue;
            };
            let Some(target) = resolve_import(&module.path, specifier, &known) else {
                continue;
            };
            mounted.entry(target).or_default().push(prefix.clone());
        }
    }
    for prefixes in mounted.values_mut() {
        prefixes.sort();
        prefixes.dedup();
    }
    mounted
}

fn resolve_import(importer: &Path, specifier: &str, known: &BTreeSet<PathBuf>) -> Option<PathBuf> {
    if !specifier.starts_with("./") && !specifier.starts_with("../") {
        return None;
    }
    let base = importer.parent()?.join(specifier);
    let mut candidates = vec![base.clone()];
    for extension in ["js", "mjs", "cjs", "ts", "tsx", "jsx"] {
        candidates.push(base.with_extension(extension));
    }
    for extension in ["js", "mjs", "cjs", "ts", "tsx", "jsx"] {
        candidates.push(base.join(format!("index.{extension}")));
    }
    candidates
        .into_iter()
        .find(|candidate| known.contains(candidate))
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

struct WalkState<'a> {
    routes: &'a mut Vec<RawRoute>,
    shapes: &'a mut BTreeMap<String, BTreeMap<String, FieldFact>>,
    queries: &'a mut BTreeMap<String, BTreeMap<String, FieldFact>>,
    ambiguous: &'a mut BTreeSet<String>,
    mounts: &'a mut BTreeMap<String, String>,
    imports: &'a mut BTreeMap<String, String>,
    routers: &'a mut BTreeSet<String>,
}

fn walk(node: Node, text: &str, server: bool, state: &mut WalkState<'_>) {
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
                        state
                            .routes
                            .push((object.clone(), path, method, handler, None));
                    }
                }
                // A prefix-less mount (`app.use(router)`, koa's
                // `app.use(router.routes())`) mounts the router at the root.
                // Without recording it, a locally declared AND locally mounted
                // router read as "exported for another module to mount" and
                // every one of its routes was dropped; the koa idiom never
                // passes a path string, so the whole family was inert on real
                // programs while its path-carrying snippets kept passing.
                if property == "use" && first.is_none() {
                    if let Some(name) = args.first().and_then(|node| mounted_router(*node, text)) {
                        state.mounts.entry(name).or_default();
                    }
                }
                if let Some(path) = first.filter(|path| path.starts_with('/')) {
                    if property == "use" {
                        if let Some(name) = args.get(1).and_then(|node| identifier(*node, text)) {
                            state.mounts.insert(name, path);
                        } else if let Some(specifier) =
                            args.get(1).and_then(|node| require_specifier(*node, text))
                        {
                            let key = format!("@module:{specifier}");
                            state.imports.insert(key.clone(), specifier);
                            state.mounts.insert(key, path);
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
                        // No named handler and no schema: a plain inline
                        // handler still states its field names in its own
                        // source (`const { name } = req.body`, `req.query.q`),
                        // which is what the probe planner synthesizes an honest
                        // request from and what names the query parameter in
                        // the draft.
                        let handler = handler.or_else(|| {
                            args.iter().skip(1).rev().find_map(|argument| {
                                let read = super::node_body::inline_request(*argument, text)?;
                                let name = format!("{method} {path} inline handler");
                                if !read.body.is_empty() {
                                    super::field_facts::record(
                                        state.shapes,
                                        state.ambiguous,
                                        name.clone(),
                                        read.body,
                                    );
                                }
                                if !read.query.is_empty() {
                                    state.queries.insert(name.clone(), read.query);
                                }
                                Some(name)
                            })
                        });
                        state.routes.push((object, path, *method, handler, schema));
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
        nest_controller(node, text, state.routes);
    }
    if node.kind() == "variable_declarator" {
        if let (Some(name), Some(value)) = (
            node.child_by_field_name("name")
                .and_then(|n| n.utf8_text(text.as_bytes()).ok())
                .map(str::to_string),
            node.child_by_field_name("value"),
        ) {
            if is_router_constructor(value, text) {
                state.routers.insert(name.clone());
            }
            if let Some(specifier) = require_specifier(value, text) {
                state.imports.insert(name.clone(), specifier);
            }
            if let Some(fields) = super::node_body::zod_object(value, text) {
                match state.shapes.get(&name) {
                    Some(existing) if *existing != fields => {
                        state.ambiguous.insert(name);
                    }
                    Some(_) => {}
                    None => {
                        state.shapes.insert(name, fields);
                    }
                }
            }
        }
    }
    if node.kind() == "import_statement" {
        collect_imports(node, text, state.imports);
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk(child, text, server, state);
    }
}

fn is_router_constructor(node: Node, text: &str) -> bool {
    let compact: String = grammar_text(node, text)
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect();
    compact.ends_with("Router()") || compact.ends_with("Router({})")
}

fn require_specifier(node: Node, text: &str) -> Option<String> {
    if node.kind() != "call_expression"
        || grammar::field(node, text, "function").as_deref() != Some("require")
    {
        return None;
    }
    let arguments = node.child_by_field_name("arguments")?;
    let mut children = Vec::new();
    grammar_children(arguments, &mut children);
    let specifier = children
        .first()
        .and_then(|child| string_value(*child, text))?;
    (specifier.starts_with("./") || specifier.starts_with("../")).then_some(specifier)
}

fn collect_imports(node: Node, text: &str, imports: &mut BTreeMap<String, String>) {
    let Some(source) = node
        .child_by_field_name("source")
        .and_then(|source| string_value(source, text))
    else {
        return;
    };
    if !source.starts_with("./") && !source.starts_with("../") {
        return;
    }
    let Some(clause) = node
        .named_children(&mut node.walk())
        .find(|child| child.kind() == "import_clause")
    else {
        return;
    };
    grammar::walk(clause, &mut |child| {
        if child.kind() == "identifier" {
            imports.insert(grammar_text(child, text).to_string(), source.clone());
        }
    });
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

fn grammar_children<'a>(node: Node<'a>, into: &mut Vec<Node<'a>>) {
    super::grammar::children(node, into);
}

fn grammar_text<'a>(node: Node, source: &'a str) -> &'a str {
    super::grammar::text(node, source)
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

/// The router a prefix-less `.use(...)` argument mounts: a bare identifier
/// (`app.use(router)`) or the object of a call chain, which is how koa mounts
/// (`app.use(router.routes())`, `app.use(router.allowedMethods())`).
fn mounted_router(node: Node, text: &str) -> Option<String> {
    if let Some(name) = identifier(node, text) {
        return Some(name);
    }
    if node.kind() != "call_expression" {
        return None;
    }
    let callee = node.child_by_field_name("function")?;
    if callee.kind() != "member_expression" {
        return None;
    }
    identifier(callee.child_by_field_name("object")?, text)
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
    fn an_inline_handler_states_its_query_parameters_and_its_body_fields() {
        let source = read_source(
            "inline-request",
            &[(
                "server.js",
                "const express = require('express');\nconst app = express();\n\
                 app.get('/search', (req, res) => {\n\
                 \x20 const { q } = req.query;\n\
                 \x20 res.json(search(q));\n\
                 });\n\
                 app.post('/items', (req, res) => {\n\
                 \x20 const { name, price } = req.body;\n\
                 \x20 res.json({ name: name.trim(), price });\n\
                 });\n",
            )],
        );
        let search = "get /search inline handler";
        let create = "post /items inline handler";
        assert_eq!(
            source
                .queries
                .get(search)
                .map(|fields| fields.keys().cloned().collect::<Vec<_>>()),
            Some(vec!["q".to_string()]),
            "the query parameter the handler branches on must be named"
        );
        assert!(
            !source.bodies.contains_key(search),
            "a GET that reads no body must not be given one"
        );
        assert_eq!(
            source
                .bodies
                .get(create)
                .map(|fields| fields.keys().cloned().collect::<Vec<_>>()),
            Some(vec!["name".to_string(), "price".to_string()])
        );
        assert!(!source.queries.contains_key(create));
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
    fn a_router_mounted_from_another_module_has_no_unmounted_paths() {
        let source = read_source(
            "cross-file-mount",
            &[
                (
                    "server.js",
                    "const express = require('express');\n\
                     const users = require('./users');\n\
                     const app = express();\n\
                     app.use('/api', users);\n",
                ),
                (
                    "users.js",
                    "const express = require('express');\n\
                     const router = express.Router();\n\
                     router.get('/users', listUsers);\n\
                     module.exports = router;\n",
                ),
            ],
        );
        assert_eq!(
            source.routes,
            vec![("/api/users".to_string(), "get", Some("listUsers".into()))]
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
