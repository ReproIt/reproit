//! Python route and type extraction over the tree-sitter grammar.
//!
//! Pydantic and the routing decorators are the most declarative surface of any
//! family here, which made the pattern reader work well enough to hide that it
//! was still guessing. Over a parse the decorator, the handler it decorates, and
//! the annotated parameter are one structure rather than three lines that
//! happened to be adjacent, so a wrapped decorator, a comment between them, or a
//! nested definition stop mattering.

use super::django_urls::{collect_view_verbs, django_routes};
use super::field_facts::FieldFact;
use super::grammar::SourceRead;

/// One route before its module's mount prefix is applied.
pub(super) type PyRoute = (String, &'static str, Option<String>);
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use tree_sitter::{Node, Parser};

const METHODS: [&str; 7] = ["get", "post", "put", "patch", "delete", "head", "options"];
/// Bound the class body scanned for fields.
const MAX_FIELDS: usize = 512;

pub(super) fn read(root: &Path) -> SourceRead {
    let mut source = SourceRead::default();
    let mut parser = Parser::new();
    if parser
        .set_language(&tree_sitter_python::LANGUAGE.into())
        .is_err()
    {
        return source;
    }
    let mut models: BTreeMap<String, BTreeMap<String, FieldFact>> = BTreeMap::new();
    let mut enums: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut ambiguous: BTreeSet<String> = BTreeSet::new();
    let mut handler_models: Vec<(String, String)> = Vec::new();
    // MODULE -> the prefix another file mounts it under, from
    // `include_router(users.router, prefix="/v1")`. This one is genuinely
    // cross-file, and the dotted argument names the module, so it resolves
    // without guessing.
    let mut mounted: BTreeMap<String, String> = BTreeMap::new();
    // Modules mounted under a prefix that is NOT a literal (a settings
    // constant, say). Their real paths are unknowable from source, so their
    // routes are dropped rather than emitted one prefix short.
    let mut opaque: BTreeSet<String> = BTreeSet::new();
    let mut pending: Vec<(String, Vec<PyRoute>)> = Vec::new();
    // view name -> the verbs it answers, for Django, where the URL states none.
    let mut view_verbs: BTreeMap<String, Vec<&'static str>> = BTreeMap::new();
    let mut django_files: BTreeSet<String> = BTreeSet::new();

    for file in super::extract::family_sources(root, super::extract::Family::Python) {
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
        // Router bindings are LOCAL. `router = APIRouter(prefix="/items")` in
        // one module and a bare `router = APIRouter()` in another are two
        // routers sharing a name; a map that outlived the file put the first
        // one's prefix on the second one's routes and invented every path.
        let mut prefixes: BTreeMap<String, String> = BTreeMap::new();
        collect_prefixes(tree.root_node(), &text, &mut prefixes);
        collect_mounts(tree.root_node(), &text, &mut mounted, &mut opaque);
        collect_view_verbs(tree.root_node(), &text, &mut view_verbs);
        let mut found = Vec::new();
        // Django declares routes in urls.py rather than on the handler, and
        // declares no method there, so the draft claims only GET. Restricted to
        // that file so an ordinary `path(...)` helper elsewhere is not a route.
        if file.file_name().is_some_and(|name| name == "urls.py") {
            django_routes(tree.root_node(), &text, &mut found, &mut mounted);
            django_files.insert(module_of(&file));
        }
        walk(
            tree.root_node(),
            &text,
            &mut found,
            &mut models,
            &mut enums,
            &mut ambiguous,
            &mut handler_models,
            &prefixes,
        );
        // Every Django app names its route table `urls.py`, so the stem
        // collides across the whole project; the app is the DIRECTORY, which is
        // also what `include('blog.urls')` names.
        let module = module_of(&file);
        pending.push((module, found));
    }

    for (module, routes) in pending {
        // A module mounted under an unreadable prefix contributes nothing: the
        // paths it serves are real but their location is not knowable here, and
        // emitting them at the wrong place is worse than not emitting them.
        if opaque.contains(&module) {
            source.files_unreadable += 1;
            continue;
        }
        let outer = mounted.get(&module).cloned().unwrap_or_default();
        let django = django_files.contains(&module);
        for (path, method, handler) in routes {
            let path = format!("{}{path}", outer.trim_end_matches('/'));
            // Now that every file is read, a Django view's own verbs are known.
            let methods = match handler
                .as_ref()
                .filter(|_| django)
                .and_then(|name| view_verbs.get(name))
            {
                Some(found) => found.clone(),
                None => vec![method],
            };
            for method in methods {
                source.routes.push((path.clone(), method, handler.clone()));
            }
        }
    }

    // Two modules declaring the same model differently is not a verdict.
    for name in &ambiguous {
        models.remove(name);
    }
    for (handler, model) in handler_models {
        let Some(fields) = models.get(&model) else {
            continue;
        };
        let mut fields = fields.clone();
        for fact in fields.values_mut() {
            if let Some(name) = fact.evidence.as_deref().and_then(|e| e.strip_prefix('@')) {
                match enums.get(name) {
                    Some(values) => {
                        fact.allowed = Some(values.clone());
                        fact.evidence = Some("an Enum-typed annotation".to_string());
                    }
                    None => fact.evidence = None,
                }
            }
        }
        source.bodies.insert(handler, fields);
    }
    source
}

#[allow(clippy::too_many_arguments)]
fn walk(
    node: Node,
    text: &str,
    routes: &mut Vec<(String, &'static str, Option<String>)>,
    models: &mut BTreeMap<String, BTreeMap<String, FieldFact>>,
    enums: &mut BTreeMap<String, Vec<String>>,
    ambiguous: &mut BTreeSet<String>,
    handler_models: &mut Vec<(String, String)>,
    prefixes: &BTreeMap<String, String>,
) {
    match node.kind() {
        "decorated_definition" => {
            if let Some((handler, params)) = decorated_function(node, text) {
                for (router, path, method) in decorator_routes(node, text) {
                    let path = match prefixes.get(&router) {
                        Some(prefix) => format!("{}{path}", prefix.trim_end_matches('/')),
                        None => path,
                    };
                    routes.push((path, method, Some(handler.clone())));
                }
                // FastAPI infers the body from the annotated parameter, so this
                // reads the same signal the framework does.
                for (_, annotation) in params {
                    handler_models.push((handler.clone(), annotation));
                }
            }
        }
        "class_definition" => {
            let Some(name) = field_text(node, "name", text) else {
                return;
            };
            let bases = field_text(node, "superclasses", text).unwrap_or_default();
            if bases.contains("Enum") {
                let values = enum_values(node, text);
                if !values.is_empty() {
                    enums.insert(name, values);
                }
                return;
            }
            if !bases.contains("BaseModel") && !bases.contains("Schema") {
                return;
            }
            let fields = model_fields(node, text);
            if fields.is_empty() {
                return;
            }
            match models.get(&name) {
                Some(existing) if *existing != fields => {
                    ambiguous.insert(name);
                }
                Some(_) => {}
                None => {
                    models.insert(name, fields);
                }
            }
        }
        _ => {}
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk(
            child,
            text,
            routes,
            models,
            enums,
            ambiguous,
            handler_models,
            prefixes,
        );
    }
}

/// The function a decorator block defines, with its annotated parameters.
fn decorated_function(node: Node, text: &str) -> Option<(String, Vec<(String, String)>)> {
    let definition = node.child_by_field_name("definition")?;
    if definition.kind() != "function_definition" {
        return None;
    }
    let name = field_text(definition, "name", text)?;
    let mut params = Vec::new();
    if let Some(list) = definition.child_by_field_name("parameters") {
        let mut cursor = list.walk();
        for parameter in list.children(&mut cursor) {
            if parameter.kind() != "typed_parameter" {
                continue;
            }
            let annotation = parameter
                .child_by_field_name("type")
                .and_then(|node| node.utf8_text(text.as_bytes()).ok())
                .unwrap_or_default()
                .trim()
                .to_string();
            let mut inner = parameter.walk();
            let ident = parameter
                .children(&mut inner)
                .find(|child| child.kind() == "identifier")
                .and_then(|node| node.utf8_text(text.as_bytes()).ok())
                .unwrap_or_default()
                .to_string();
            if !annotation.is_empty() {
                params.push((ident, annotation));
            }
        }
    }
    Some((name, params))
}

/// `@app.post("/x")` and `@app.route("/x", methods=["GET"])`.
fn decorator_routes(node: Node, text: &str) -> Vec<(String, String, &'static str)> {
    let mut found = Vec::new();
    let mut cursor = node.walk();
    for decorator in node.children(&mut cursor) {
        if decorator.kind() != "decorator" {
            continue;
        }
        let mut inner = decorator.walk();
        let Some(call) = decorator
            .children(&mut inner)
            .find(|child| child.kind() == "call")
        else {
            continue;
        };
        let Some(attribute) = call.child_by_field_name("function") else {
            continue;
        };
        let router = attribute
            .child_by_field_name("object")
            .and_then(|node| node.utf8_text(text.as_bytes()).ok())
            .unwrap_or_default()
            .to_string();
        let verb = attribute
            .child_by_field_name("attribute")
            .and_then(|node| node.utf8_text(text.as_bytes()).ok())
            .unwrap_or_default()
            .to_string();
        let Some(arguments) = call.child_by_field_name("arguments") else {
            continue;
        };
        let mut args = arguments.walk();
        let children: Vec<Node> = arguments.children(&mut args).collect();
        let Some(path) = children
            .iter()
            .find(|child| child.kind() == "string")
            .and_then(|node| string_value(*node, text))
        else {
            continue;
        };
        if let Some(method) = METHODS.iter().find(|method| **method == verb) {
            found.push((router, path, *method));
            continue;
        }
        // Flask: the verbs live in a `methods=[...]` keyword.
        if verb == "route" {
            let listed = arguments
                .utf8_text(text.as_bytes())
                .unwrap_or_default()
                .to_lowercase();
            let mut any = false;
            for method in METHODS {
                if listed.contains(&format!("\"{method}\""))
                    || listed.contains(&format!("'{method}'"))
                {
                    found.push((router.clone(), path.clone(), method));
                    any = true;
                }
            }
            if !any {
                found.push((router, path, "get"));
            }
        }
    }
    found
}

fn model_fields(node: Node, text: &str) -> BTreeMap<String, FieldFact> {
    let mut fields = BTreeMap::new();
    let Some(body) = node.child_by_field_name("body") else {
        return fields;
    };
    let mut cursor = body.walk();
    for statement in body.children(&mut cursor).take(MAX_FIELDS) {
        // `name: annotation` and `name: annotation = default`.
        let expression = match statement.kind() {
            "expression_statement" => statement.child(0),
            _ => None,
        };
        let Some(expression) = expression else {
            continue;
        };
        let (annotation_node, default) = match expression.kind() {
            "type" | "typed_parameter" => (Some(expression), None),
            "assignment" => (
                expression.child_by_field_name("type"),
                expression.child_by_field_name("right"),
            ),
            _ => continue,
        };
        let Some(annotation_node) = annotation_node else {
            continue;
        };
        let Some(name) = expression
            .child_by_field_name("left")
            .or_else(|| expression.child(0))
            .and_then(|node| node.utf8_text(text.as_bytes()).ok())
            .map(str::to_string)
        else {
            continue;
        };
        if !name.chars().next().is_some_and(char::is_alphabetic) {
            continue;
        }
        let annotation = annotation_node
            .utf8_text(text.as_bytes())
            .unwrap_or_default()
            .to_string();
        let default_text = default
            .and_then(|node| node.utf8_text(text.as_bytes()).ok())
            .unwrap_or_default()
            .to_string();
        fields.insert(name, fact(&annotation, &default_text));
    }
    fields
}

fn fact(annotation: &str, default: &str) -> FieldFact {
    let optional = annotation.contains("Optional[")
        || annotation.contains("| None")
        || annotation.contains("None |")
        || (!default.is_empty() && !default.starts_with("Field("))
        || default.contains("default");
    let allowed = annotation
        .split_once("Literal[")
        .and_then(|(_, rest)| rest.split_once(']'))
        .and_then(|(inner, _)| literal_values(inner));
    let range = field_bounds(default);
    FieldFact {
        required: !optional,
        evidence: match (&allowed, &range) {
            (Some(_), _) => Some("a Literal annotation".to_string()),
            (_, Some(_)) => Some("a Field(...) bound".to_string()),
            // Remembered by name; resolved once every module has been read.
            _ => Some(format!("@{}", annotation.trim())),
        },
        allowed,
        range,
    }
}

/// `Field(ge=-1, le=1)`, with exclusive bounds converted to what is accepted.
fn field_bounds(default: &str) -> Option<(Option<f64>, Option<f64>)> {
    let compact: String = default.chars().filter(|c| !c.is_whitespace()).collect();
    let mut low = None;
    let mut high = None;
    for (key, slot) in [("ge=", 0), ("gt=", 1), ("le=", 2), ("lt=", 3)] {
        let Some(value) = compact.split(key).nth(1) else {
            continue;
        };
        let literal: String = value
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '-' || *c == '.')
            .collect();
        let Ok(number) = literal.parse::<f64>() else {
            continue;
        };
        match slot {
            0 => low = Some(number),
            1 => low = Some(number + 1.0),
            2 => high = Some(number),
            _ => high = Some(number - 1.0),
        }
    }
    (low.is_some() || high.is_some()).then_some((low, high))
}

fn enum_values(node: Node, text: &str) -> Vec<String> {
    let Some(body) = node.child_by_field_name("body") else {
        return Vec::new();
    };
    let mut values = Vec::new();
    let mut cursor = body.walk();
    for statement in body.children(&mut cursor).take(MAX_FIELDS) {
        let Some(assignment) = statement.child(0) else {
            continue;
        };
        if assignment.kind() != "assignment" {
            continue;
        }
        if let Some(value) = assignment
            .child_by_field_name("right")
            .and_then(|node| string_value(node, text))
        {
            values.push(value);
        }
    }
    values
}

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

pub(super) fn string_value(node: Node, text: &str) -> Option<String> {
    let raw = node.utf8_text(text.as_bytes()).ok()?;
    Some(
        raw.trim_start_matches(['r', 'f', 'b'])
            .trim_matches(['"', '\''])
            .to_string(),
    )
}

fn field_text(node: Node, field: &str, text: &str) -> Option<String> {
    node.child_by_field_name(field)?
        .utf8_text(text.as_bytes())
        .ok()
        .map(str::to_string)
}

/// The module key for a source file: the app directory for a Django `urls.py`,
/// whose stem collides across the whole project, and the stem otherwise.
fn module_of(file: &std::path::Path) -> String {
    if file.file_name().is_some_and(|name| name == "urls.py") {
        // The dotted module path an `include(...)` would name, so
        // `ipam/api/urls.py` is `ipam.api` rather than the ambiguous `api`.
        let parts: Vec<String> = file
            .parent()
            .map(|dir| {
                dir.components()
                    .map(|part| part.as_os_str().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        let tail: Vec<String> = parts.into_iter().rev().take(2).collect();
        return tail.into_iter().rev().collect::<Vec<_>>().join(".");
    }
    file.file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Cross-file mounts: `include_router(items.router, prefix="/api")`.
///
/// The dotted argument names the MODULE, which is what makes this resolvable
/// across files at all. A `prefix=` whose value is not a literal marks the
/// module opaque rather than being ignored, because a route emitted one prefix
/// short is a path the service does not serve.
fn collect_mounts(
    node: Node,
    text: &str,
    mounted: &mut BTreeMap<String, String>,
    opaque: &mut BTreeSet<String>,
) {
    if node.kind() == "call" {
        let callee = node
            .child_by_field_name("function")
            .and_then(|node| node.utf8_text(text.as_bytes()).ok())
            .unwrap_or_default();
        if callee.ends_with("include_router") {
            if let Some(arguments) = node.child_by_field_name("arguments") {
                let mut cursor = arguments.walk();
                let first = arguments
                    .children(&mut cursor)
                    .find(|child| child.is_named())
                    .and_then(|node| node.utf8_text(text.as_bytes()).ok())
                    .unwrap_or_default()
                    .to_string();
                // `items.router` names the module; a bare `router` does not.
                if let Some((module, _)) = first.split_once('.') {
                    let module = module.trim().to_string();
                    match keyword_string(node, text, &["prefix"]) {
                        Some(prefix) => {
                            mounted.insert(module, prefix.trim_end_matches('/').to_string());
                        }
                        None if node
                            .utf8_text(text.as_bytes())
                            .unwrap_or_default()
                            .contains("prefix=") =>
                        {
                            opaque.insert(module);
                        }
                        None => {}
                    }
                }
            }
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_mounts(child, text, mounted, opaque);
    }
}

/// Mount prefixes by router variable.
///
/// `bp = Blueprint(..., url_prefix='/api/v1')` and
/// `r = APIRouter(prefix='/api')` both attach a prefix to a name that
/// decorators then hang off, and `include_router(r, prefix='/v1')` adds
/// another outside it. Resolved per file, because that is where the binding is
/// visible; a router mounted from elsewhere keeps its local path rather than a
/// fabricated one.
fn collect_prefixes(node: Node, text: &str, prefixes: &mut BTreeMap<String, String>) {
    if node.kind() == "assignment" {
        if let (Some(name), Some(value)) = (
            node.child_by_field_name("left")
                .and_then(|node| node.utf8_text(text.as_bytes()).ok()),
            node.child_by_field_name("right"),
        ) {
            let call = value.utf8_text(text.as_bytes()).unwrap_or_default();
            if call.contains("Blueprint(") || call.contains("APIRouter(") {
                if let Some(prefix) = keyword_string(value, text, &["url_prefix", "prefix"]) {
                    prefixes.insert(name.trim().to_string(), prefix);
                }
            }
        }
    }
    // `include_router(users, prefix="/v1")` composes OUTSIDE any prefix the
    // router already carries.
    if node.kind() == "call" {
        let callee = node
            .child_by_field_name("function")
            .and_then(|node| node.utf8_text(text.as_bytes()).ok())
            .unwrap_or_default();
        if callee.ends_with("include_router") {
            if let Some(arguments) = node.child_by_field_name("arguments") {
                let mut cursor = arguments.walk();
                let router = arguments
                    .children(&mut cursor)
                    .find(|child| child.kind() == "identifier")
                    .and_then(|node| node.utf8_text(text.as_bytes()).ok())
                    .unwrap_or_default()
                    .to_string();
                if let Some(outer) = keyword_string(node, text, &["prefix"]) {
                    let inner = prefixes.get(&router).cloned().unwrap_or_default();
                    prefixes.insert(router, format!("{}{inner}", outer.trim_end_matches('/')));
                }
            }
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_prefixes(child, text, prefixes);
    }
}

/// The string value of the first matching keyword argument.
fn keyword_string(node: Node, text: &str, keys: &[&str]) -> Option<String> {
    let raw = node.utf8_text(text.as_bytes()).ok()?;
    for key in keys {
        let Some((_, rest)) = raw.split_once(&format!("{key}=")) else {
            continue;
        };
        let trimmed = rest.trim_start();
        let quote = trimmed.chars().next()?;
        if quote != '"' && quote != '\'' {
            continue;
        }
        let value: String = trimmed[1..].chars().take_while(|c| *c != quote).collect();
        if !value.is_empty() {
            return Some(value);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_source(case: &str, files: &[(&str, &str)]) -> SourceRead {
        let root =
            std::env::temp_dir().join(format!("reproit-pyast-{}-{case}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("root");
        for (name, body) in files {
            std::fs::write(root.join(name), body).expect("write");
        }
        let source = read(&root);
        let _ = std::fs::remove_dir_all(&root);
        source
    }

    const APP: &str = r#"
from pydantic import BaseModel, Field
from typing import Literal, Optional

class BlockRequest(BaseModel):
    blocked_type: Literal["user", "sponsor"]
    blocked_id: str
    rating: int = Field(ge=-1, le=1)
    note: Optional[str] = None

@app.post("/v1/blocks")
async def create_block(body: BlockRequest):
    return {}
"#;

    #[test]
    fn a_router_prefix_does_not_leak_into_another_module() {
        // A prefix-less `APIRouter()` in users.py inherited `/items` from
        // items.py, inventing /items/users/me and losing /users/me.
        let source = read_source(
            "no_leak",
            &[
                (
                    "items.py",
                    "from fastapi import APIRouter\nrouter = APIRouter(prefix=\"/items\")\n\
                     @router.get(\"/{item_id}\")\nasync def read_item(item_id: str): return {}\n",
                ),
                (
                    "users.py",
                    "from fastapi import APIRouter\nrouter = APIRouter()\n\
                     @router.get(\"/users/me\")\nasync def me(): return {}\n",
                ),
            ],
        );
        let paths: Vec<&String> = source.routes.iter().map(|(path, _, _)| path).collect();
        assert!(
            paths.contains(&&"/items/{item_id}".to_string()),
            "{paths:?}"
        );
        assert!(paths.contains(&&"/users/me".to_string()), "{paths:?}");
        assert!(
            !paths.contains(&&"/items/users/me".to_string()),
            "a neighbour's prefix must not be applied: {paths:?}"
        );
    }

    #[test]
    fn a_module_mounted_under_an_unreadable_prefix_abstains() {
        // `include_router(api_router, prefix=settings.API_V1_STR)`: the routes
        // are real but their location is not knowable, and emitting them one
        // prefix short names paths the service does not serve.
        let source = read_source(
            "opaque_mount",
            &[
                (
                    "main.py",
                    "from fastapi import FastAPI\nfrom . import api\napp = FastAPI()\n\
                     app.include_router(api.router, prefix=settings.API_V1_STR)\n",
                ),
                (
                    "api.py",
                    "from fastapi import APIRouter\nrouter = APIRouter()\n\
                     @router.post(\"/login\")\nasync def login(): return {}\n",
                ),
            ],
        );
        let paths: Vec<&String> = source.routes.iter().map(|(path, _, _)| path).collect();
        assert!(
            !paths.contains(&&"/login".to_string()),
            "an unknowable prefix must abstain: {paths:?}"
        );
    }

    #[test]
    fn the_decorator_the_handler_and_its_model_are_one_structure() {
        let source = read_source("basic", &[("main.py", APP)]);
        assert_eq!(
            source.routes,
            vec![(
                "/v1/blocks".to_string(),
                "post",
                Some("create_block".into())
            )]
        );
        let fields = source.bodies.get("create_block").expect("resolved");
        assert_eq!(
            fields["blocked_type"].allowed.as_deref(),
            Some(["user".to_string(), "sponsor".to_string()].as_slice())
        );
        assert_eq!(fields["rating"].range, Some((Some(-1.0), Some(1.0))));
        assert!(!fields["note"].required);
        assert!(fields["blocked_id"].required);
    }

    #[test]
    fn a_comment_or_a_blank_line_between_decorator_and_def_changes_nothing() {
        // The pattern reader looked a fixed number of lines ahead. A parse has
        // no such window: the decorator and the function are one node.
        let source = read_source(
            "gap",
            &[(
                "main.py",
                "@app.get(\"/health\")\n# a note about this handler\n\nasync def health():\n    return {}\n",
            )],
        );
        assert_eq!(
            source.routes,
            vec![("/health".to_string(), "get", Some("health".into()))]
        );
    }

    #[test]
    fn a_flask_route_takes_its_verbs_from_the_methods_keyword() {
        let source = read_source(
            "flask",
            &[(
                "app.py",
                "@app.route(\"/things\", methods=[\"GET\", \"POST\"])\ndef things():\n    return {}\n",
            )],
        );
        let mut verbs: Vec<&str> = source.routes.iter().map(|(_, verb, _)| *verb).collect();
        verbs.sort_unstable();
        assert_eq!(verbs, vec!["get", "post"]);
    }

    #[test]
    fn a_str_enum_annotation_is_a_closed_value_set() {
        let source = read_source(
            "enum",
            &[(
                "main.py",
                "class BlockedType(str, Enum):\n    USER = \"user\"\n    SPONSOR = \"sponsor\"\n\n\
                 class R(BaseModel):\n    kind: BlockedType\n\n\
                 @app.post(\"/x\")\ndef h(body: R):\n    return {}\n",
            )],
        );
        assert_eq!(
            source.bodies.get("h").expect("resolved")["kind"]
                .allowed
                .as_deref(),
            Some(["user".to_string(), "sponsor".to_string()].as_slice())
        );
    }

    #[test]
    fn a_model_declared_differently_twice_abstains() {
        let source = read_source(
            "ambiguous",
            &[
                (
                    "models.py",
                    "class R(BaseModel):\n    a: str\n    b: str\n\n@app.post(\"/x\")\ndef h(body: R):\n    return {}\n",
                ),
                ("legacy.py", "class R(BaseModel):\n    a: str\n"),
            ],
        );
        assert!(
            !source.bodies.contains_key("h"),
            "two different models with one name is not a verdict"
        );
    }

    #[test]
    fn a_file_that_does_not_parse_is_counted_not_ignored() {
        let source = read_source(
            "broken",
            &[("good.py", "x = 1\n"), ("bad.py", "def broken(:\n")],
        );
        assert_eq!(source.files_parsed, 1);
        assert_eq!(source.files_unreadable, 1);
    }

    #[test]
    fn a_plain_class_is_not_a_request_model() {
        let source = read_source(
            "plain",
            &[(
                "main.py",
                "class Helper:\n    thing: str\n\n@app.post(\"/x\")\ndef h(body: Helper):\n    return {}\n",
            )],
        );
        assert!(!source.bodies.contains_key("h"));
    }
}
