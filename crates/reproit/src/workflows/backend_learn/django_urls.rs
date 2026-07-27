//! Django's route table, which is a different language from the decorators.
//!
//! Every other Python framework attaches its route to the handler. Django
//! declares them in a `urls.py` list, states no HTTP verb there at all, and
//! composes apps with `include()`. Reading it needs the URL regex vocabulary
//! and a way to ask the VIEW which verbs it answers, neither of which the
//! decorator reader has any use for.

use std::collections::BTreeMap;
use tree_sitter::Node;

use super::python_ast::{string_value, PyRoute};

/// The verbs a Django view answers.
///
/// `@api_view(["GET", "POST"])` on a function, and the handler methods of a
/// class-based view (`def post`), both state this exactly. Reading neither made
/// every Django route a GET, including endpoints that answer only POST.
pub(super) fn collect_view_verbs(
    node: Node,
    text: &str,
    verbs: &mut BTreeMap<String, Vec<&'static str>>,
) {
    const KNOWN: [&str; 7] = ["get", "post", "put", "patch", "delete", "head", "options"];
    if node.kind() == "decorated_definition" {
        let raw = node.utf8_text(text.as_bytes()).unwrap_or_default();
        if let Some(definition) = node.child_by_field_name("definition") {
            if let Some(name) = definition
                .child_by_field_name("name")
                .and_then(|node| node.utf8_text(text.as_bytes()).ok())
            {
                if let Some((_, rest)) = raw.split_once("@api_view") {
                    let listed: Vec<&'static str> = KNOWN
                        .into_iter()
                        .filter(|verb| {
                            let upper = verb.to_uppercase();
                            rest.split(')').next().unwrap_or_default().contains(&upper)
                        })
                        .collect();
                    if !listed.is_empty() {
                        verbs.insert(name.to_string(), listed);
                    }
                }
            }
        }
    }
    // A class-based view answers the verbs it defines handlers for.
    if node.kind() == "class_definition" {
        if let (Some(name), Some(body)) = (
            node.child_by_field_name("name")
                .and_then(|node| node.utf8_text(text.as_bytes()).ok()),
            node.child_by_field_name("body"),
        ) {
            let mut listed = Vec::new();
            let mut cursor = body.walk();
            for member in body.children(&mut cursor) {
                let definition = if member.kind() == "decorated_definition" {
                    member.child_by_field_name("definition")
                } else {
                    Some(member)
                };
                let Some(method) = definition
                    .filter(|node| node.kind() == "function_definition")
                    .and_then(|node| node.child_by_field_name("name"))
                    .and_then(|node| node.utf8_text(text.as_bytes()).ok())
                else {
                    continue;
                };
                if let Some(verb) = KNOWN.into_iter().find(|verb| *verb == method) {
                    listed.push(verb);
                }
            }
            if !listed.is_empty() {
                verbs.insert(name.to_string(), listed);
            }
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_view_verbs(child, text, verbs);
    }
}
/// Django `path("orders/", views.list)` and `re_path(...)` entries.
/// A Django URL regex as an OpenAPI template.
///
/// `^legacy/(?P<pk>\d+)/$` is `/legacy/{pk}`. Saleor declares all nine of its
/// routes with `re_path` and reading none of them made the whole service
/// surface as empty. A regex carrying anything this cannot express returns
/// None rather than a guess.
fn template_of_regex(pattern: &str) -> Option<String> {
    let mut out = String::new();
    let mut rest = pattern.trim_start_matches('^').trim_end_matches('$');
    while let Some(at) = rest.find("(?P<") {
        out.push_str(&rest[..at]);
        let after = &rest[at + 4..];
        let (name, tail) = after.split_once('>')?;
        if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return None;
        }
        out.push_str(&format!("{{{name}}}"));
        // Skip the group's body, which is the pattern the parameter matches.
        let mut depth = 1usize;
        let mut end = None;
        for (index, character) in tail.char_indices() {
            match character {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(index);
                        break;
                    }
                }
                _ => {}
            }
        }
        rest = &tail[end? + 1..];
    }
    out.push_str(rest);
    // Anything left that is regex rather than path means this is not readable.
    let readable = !out.chars().any(|c| {
        matches!(
            c,
            '(' | ')' | '[' | ']' | '+' | '*' | '?' | '\\' | '|' | '.'
        )
    });
    readable.then(|| out.replace("\\/", "/"))
}
pub(super) fn django_routes(
    node: Node,
    text: &str,
    routes: &mut Vec<PyRoute>,
    mounted: &mut BTreeMap<String, String>,
) {
    if node.kind() == "call" {
        let callee = node
            .child_by_field_name("function")
            .and_then(|node| node.utf8_text(text.as_bytes()).ok())
            .unwrap_or_default();
        if callee == "path" || callee == "re_path" {
            if let Some(arguments) = node.child_by_field_name("arguments") {
                let mut cursor = arguments.walk();
                let children: Vec<Node> = arguments.children(&mut cursor).collect();
                let raw = children.iter().find(|child| child.kind() == "string");
                // An f-string interpolates a value this cannot know.
                // `path(f"{base_url}/", ...)` became the literal route
                // `/{base_url}`, which is a path nothing serves.
                let interpolated =
                    raw.is_some_and(|node| super::grammar::find(*node, "interpolation").is_some());
                let path = raw
                    .filter(|_| !interpolated)
                    .and_then(|node| string_value(*node, text));
                // `path('blog/', include('blog.urls'))` is a MOUNT, not an
                // endpoint. Emitting it as a leaf GET invented a route that
                // 404s, and the module it mounts was read unprefixed.
                // `include('blog.urls')` names the app whose table this
                // mounts, which is what makes the prefix resolvable across
                // files at all.
                let included = children.iter().find_map(|child| {
                    let raw = child.utf8_text(text.as_bytes()).unwrap_or_default();
                    let inner = raw.strip_prefix("include(")?;
                    let quoted = inner.trim_start_matches(['(', '\'', '"']);
                    let module = quoted
                        .split(['\'', '"', ',', ')'])
                        .next()
                        .unwrap_or_default();
                    // The WHOLE dotted module, minus the trailing `urls`.
                    // Keying on one component made `ipam.api.urls` and
                    // `users.api.urls` both "api", so every REST include landed
                    // under whichever app was mounted last.
                    let app = module
                        .split('.')
                        .filter(|part| !part.is_empty() && *part != "urls")
                        .collect::<Vec<_>>()
                        .join(".");
                    (!app.is_empty()).then_some(app)
                });
                // `views.foo` names the function; `views.Foo.as_view()` names
                // the CLASS, one component further in, and it arrives as a call
                // rather than an attribute so it was not read at all.
                let handler = children
                    .iter()
                    .filter(|child| matches!(child.kind(), "attribute" | "identifier" | "call"))
                    .find_map(|child| {
                        let raw = child.utf8_text(text.as_bytes()).ok()?;
                        let named = raw.strip_suffix("()").unwrap_or(raw);
                        let named = named.strip_suffix(".as_view").unwrap_or(named);
                        let last = named.rsplit('.').next()?.trim();
                        (!last.is_empty() && last != "include").then(|| last.to_string())
                    });
                if let Some(path) = path {
                    let path = if callee == "re_path" {
                        match template_of_regex(&path) {
                            Some(path) => path,
                            // A regex this cannot express as a template is not
                            // guessed at.
                            None => return,
                        }
                    } else {
                        path
                    };
                    let path = format!("/{}", path.trim_matches('/'));
                    match included {
                        Some(app) => {
                            mounted.insert(app, path.trim_end_matches('/').to_string());
                        }
                        // Django states no verb at the URL. Which verbs the
                        // VIEW answers is knowable, but only once every file
                        // has been read: `urls.py` sorts before `views.py`, so
                        // resolving here found an empty map and made every
                        // Django route a GET, including endpoints that only
                        // answer POST.
                        None => routes.push((path, "get", handler)),
                    }
                }
            }
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        django_routes(child, text, routes, mounted);
    }
}
