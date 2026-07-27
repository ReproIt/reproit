//! Laravel route and validation-rule extraction over the grammar.
//!
//! A Laravel rule set is a PHP array literal returned from `rules()`, and the
//! pattern reader matched its entries one line at a time. That works until the
//! array uses the multi-line `['required', 'in:a,b']` form, or a rule string
//! contains a comma the line pattern reads as an entry boundary. Over a parse
//! an array element is an array element, in either form.
//!
//! Route groups are the other tree fact: `Route::prefix('/v1')->group(...)`
//! prefixes its body, which no single line states.

use super::extract::Family;
use super::field_facts::{drop_ambiguous, record, FieldFact};
use super::grammar::{self, SourceRead, MAX_FIELDS};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use tree_sitter::Node;

const METHODS: [&str; 7] = ["get", "post", "put", "patch", "delete", "head", "options"];

pub(super) fn read(root: &Path) -> SourceRead {
    let mut source = SourceRead::default();
    // FormRequest / controller class -> the fields its `rules()` declares.
    let mut shapes: BTreeMap<String, BTreeMap<String, FieldFact>> = BTreeMap::new();
    let mut ambiguous: BTreeSet<String> = BTreeSet::new();
    let mut found: Vec<(String, &'static str, Option<String>)> = Vec::new();

    grammar::read_files(
        root,
        Family::Php,
        tree_sitter_php::LANGUAGE_PHP.into(),
        &mut source,
        |root_node, text| {
            grammar::walk(root_node, &mut |node| {
                if node.kind() == "class_declaration" {
                    collect_class(node, text, &mut shapes, &mut ambiguous);
                }
            });
            routes_under(root_node, text, "", &mut found);
        },
    );
    drop_ambiguous(&mut shapes, &ambiguous);

    for (path, method, handler) in found {
        source.routes.push((path, method, handler.clone()));
        if let Some(handler) = handler {
            if let Some(fields) = shapes.get(&handler).cloned() {
                source.bodies.insert(handler, fields);
            }
        }
    }
    source
}

/// Routes under `prefix`, descending into the group blocks that extend it.
///
/// `Route::prefix('v1')->group(...)` and `Route::group(['prefix' => 'v2'], ...)`
/// both nest their body under a prefix, which no single statement states. A
/// flat walk read the inner routes at the wrong path.
fn routes_under(
    node: Node,
    text: &str,
    prefix: &str,
    out: &mut Vec<(String, &'static str, Option<String>)>,
) {
    let mut children = Vec::new();
    grammar::children(node, &mut children);
    for child in children {
        if let Some((inner, body)) = group_of(child, text, prefix) {
            routes_under(body, text, &inner, out);
            continue;
        }
        if child.kind() == "scoped_call_expression" {
            collect_route(child, text, prefix, out);
        }
        routes_under(child, text, prefix, out);
    }
}

/// A `->group(...)` call and the prefix its body inherits, if this is one.
fn group_of<'a>(node: Node<'a>, text: &str, outer: &str) -> Option<(String, Node<'a>)> {
    // `Route::prefix('v1')->group(..)` is a member call; `Route::group([..], ..)`
    // is a scoped one. Both nest a body under a prefix.
    if !matches!(
        node.kind(),
        "member_call_expression" | "scoped_call_expression"
    ) {
        return None;
    }
    if grammar::field(node, text, "name").as_deref() != Some("group") {
        return None;
    }
    let raw = grammar::text(node, text);
    // `Route::prefix('v1')->...->group(...)`, anywhere in the chain.
    let chained = raw
        .split_once("prefix(")
        .map(|(_, rest)| rest)
        .and_then(|rest| rest.split_once(')'))
        .map(|(inner, _)| grammar::unquote(inner.trim()).to_string());
    // `Route::group(['prefix' => 'v2'], ...)`, the array form.
    let arrayed = raw
        .split_once("'prefix'")
        .or_else(|| raw.split_once("\"prefix\""))
        .map(|(_, rest)| rest)
        .and_then(|rest| rest.split_once("=>"))
        .map(|(_, rest)| rest)
        .and_then(|rest| {
            let rest = rest.trim_start();
            let quote = rest.chars().next()?;
            (quote == '\'' || quote == '"').then(|| rest[1..].split(quote).next())?
        })
        .map(str::to_string);
    let inner = chained.or(arrayed).unwrap_or_default();
    let composed = join(outer, &inner);
    let body = node.child_by_field_name("arguments")?;
    Some((composed, body))
}

/// Compose two Laravel path fragments, either of which may omit its slashes.
fn join(outer: &str, inner: &str) -> String {
    let outer = outer.trim_matches('/');
    let inner = inner.trim_matches('/');
    match (outer.is_empty(), inner.is_empty()) {
        (true, true) => String::new(),
        (true, false) => format!("/{inner}"),
        (false, true) => format!("/{outer}"),
        (false, false) => format!("/{outer}/{inner}"),
    }
}

/// The five operations `apiResource` declares, and the two `resource` adds.
const API_RESOURCE: [(&str, &str); 5] = [
    ("", "get"),
    ("", "post"),
    ("/{id}", "get"),
    ("/{id}", "put"),
    ("/{id}", "delete"),
];

/// `Route::post('/v1/blocks', [StoreBlockRequest::class, 'store'])`.
///
/// The handler recorded is the CLASS, not the method: Laravel states the body
/// contract on the form request or controller class, so that is the name a
/// field lookup has to resolve.
fn collect_route(
    node: Node,
    text: &str,
    prefix: &str,
    out: &mut Vec<(String, &'static str, Option<String>)>,
) {
    let mut parts = Vec::new();
    grammar::children(node, &mut parts);
    // `Route::post(...)`: scope, name, arguments.
    let (Some(scope), Some(name)) = (parts.first(), parts.get(1)) else {
        return;
    };
    if grammar::text(*scope, text) != "Route" {
        return;
    }
    let called = grammar::text(*name, text).to_ascii_lowercase();
    let Some(arguments) = node.child_by_field_name("arguments") else {
        return;
    };
    let mut args = Vec::new();
    grammar::children(arguments, &mut args);
    let first = args
        .first()
        .map(|node| grammar::unquote(grammar::text(*node, text)).to_string());

    // `Route::apiResource('users', C::class)` declares five operations and
    // `resource` seven. Reading neither lost them entirely.
    if called == "apiresource" || called == "resource" {
        if let Some(name) = first {
            let base = join(prefix, &name);
            let handler = args.get(1).and_then(|node| class_of(*node, text));
            for (suffix, method) in API_RESOURCE {
                out.push((format!("{base}{suffix}"), method, handler.clone()));
            }
            if called == "resource" {
                out.push((format!("{base}/create"), "get", handler.clone()));
                out.push((format!("{base}/{{id}}/edit"), "get", handler));
            }
        }
        return;
    }
    // `Route::any` serves every verb; a draft can only exercise one, and GET is
    // the one that cannot mutate anything if the guess is wrong.
    let method = if called == "any" {
        Some("get")
    } else {
        METHODS.into_iter().find(|known| *known == called)
    };
    // Laravel route paths are as often written without a leading slash as with
    // one (`Route::get('closeBeta', ...)` serves `/closeBeta`). Requiring one
    // dropped 326 of 328 routes in a real application.
    if let (Some(method), Some(path)) = (method, first) {
        out.push((
            join(prefix, &path),
            method,
            args.get(1).and_then(|node| class_of(*node, text)),
        ));
    }
}

/// The class an action reference names: `[StoreBlockRequest::class, 'store']`
/// or the legacy `'HealthController@index'` string.
fn class_of(node: Node, text: &str) -> Option<String> {
    let raw = grammar::text(node, text);
    if let Some(access) = grammar::find(node, "class_constant_access_expression") {
        let named = grammar::text(access, text);
        return named
            .split("::")
            .next()
            .map(|name| name.rsplit('\\').next().unwrap_or(name).to_string());
    }
    let unquoted = grammar::unquote(raw);
    let name = unquoted.split('@').next()?;
    let name = name.rsplit('\\').next().unwrap_or(name);
    let valid = !name.is_empty()
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && name.starts_with(|c: char| c.is_ascii_uppercase());
    valid.then(|| name.to_string())
}

/// A class whose `rules()` returns a Laravel rule array.
fn collect_class(
    node: Node,
    text: &str,
    shapes: &mut BTreeMap<String, BTreeMap<String, FieldFact>>,
    ambiguous: &mut BTreeSet<String>,
) {
    let Some(name) = grammar::field(node, text, "name") else {
        return;
    };
    let Some(body) = node.child_by_field_name("body") else {
        return;
    };
    let mut methods = Vec::new();
    grammar::children(body, &mut methods);
    for method in methods {
        if method.kind() != "method_declaration" {
            continue;
        }
        if grammar::field(method, text, "name").as_deref() != Some("rules") {
            continue;
        }
        let Some(array) = grammar::find(method, "array_creation_expression") else {
            continue;
        };
        let fields = rule_array(array, text);
        if !fields.is_empty() {
            record(shapes, ambiguous, name.clone(), fields);
        }
        return;
    }
}

/// `['blocked_type' => 'required|in:user,sponsor', 'n' => ['required', 'min:1']]`
fn rule_array(array: Node, text: &str) -> BTreeMap<String, FieldFact> {
    let mut fields = BTreeMap::new();
    let mut entries = Vec::new();
    grammar::children(array, &mut entries);
    for entry in entries.into_iter().take(MAX_FIELDS) {
        if entry.kind() != "array_element_initializer" {
            continue;
        }
        let mut parts = Vec::new();
        grammar::children(entry, &mut parts);
        let (Some(key), Some(value)) = (parts.first(), parts.get(1)) else {
            continue;
        };
        let name = grammar::unquote(grammar::text(*key, text)).to_string();
        // A nested rule key (`user.name`) constrains a sub-object, which the
        // flat comparison this feeds cannot express. Skipping is honest; a
        // top-level `user` fact derived from it would not be.
        if name.is_empty() || name.contains('.') || name.contains('*') {
            continue;
        }
        // Both Laravel forms mean the same thing: a pipe string or an array of
        // the same rules. Flattening here keeps one rule reader.
        let rules: Vec<String> = if value.kind() == "array_creation_expression" {
            let mut items = Vec::new();
            grammar::children(*value, &mut items);
            items
                .iter()
                .map(|item| grammar::unquote(grammar::text(*item, text)).to_string())
                .collect()
        } else {
            grammar::unquote(grammar::text(*value, text))
                .split('|')
                .map(str::to_string)
                .collect()
        };
        fields.insert(name, fact(&rules));
    }
    fields
}

fn fact(rules: &[String]) -> FieldFact {
    let mut allowed = None;
    let mut low = None;
    let mut high = None;
    let mut required = false;
    for rule in rules {
        let rule = rule.trim();
        if rule == "required" {
            required = true;
        } else if let Some(values) = rule.strip_prefix("in:") {
            let values: Vec<String> = values
                .split(',')
                .map(|value| grammar::unquote(value.trim()).to_string())
                .filter(|value| !value.is_empty())
                .collect();
            if values.len() > 1 {
                allowed = Some(values);
            }
        } else if let Some(value) = rule.strip_prefix("min:") {
            low = grammar::number(value);
        } else if let Some(value) = rule.strip_prefix("max:") {
            high = grammar::number(value);
        } else if let Some(value) = rule.strip_prefix("between:") {
            if let Some((from, to)) = value.split_once(',') {
                low = grammar::number(from);
                high = grammar::number(to);
            }
        }
    }
    let range = (low.is_some() || high.is_some()).then_some((low, high));
    FieldFact {
        required,
        evidence: match (&allowed, &range) {
            (Some(_), _) => Some("a Laravel `in:` rule".to_string()),
            (_, Some(_)) => Some("a Laravel min/max rule".to_string()),
            _ => None,
        },
        allowed,
        range,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_source(case: &str, files: &[(&str, &str)]) -> SourceRead {
        let root =
            std::env::temp_dir().join(format!("reproit-phpast-{}-{case}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("root");
        for (name, body) in files {
            std::fs::write(root.join(name), body).expect("write");
        }
        let source = read(&root);
        let _ = std::fs::remove_dir_all(&root);
        source
    }

    const ROUTES: &str = "<?php\nRoute::post('/v1/blocks', [StoreBlockRequest::class, 'store']);\n";

    #[test]
    fn a_route_resolves_the_form_request_it_names() {
        let source = read_source(
            "rules",
            &[
                ("routes.php", ROUTES),
                (
                    "StoreBlockRequest.php",
                    "<?php\nclass StoreBlockRequest extends FormRequest\n{\n\
                     \x20   public function rules()\n    {\n        return [\n\
                     \x20           'blocked_type' => 'required|in:user,sponsor',\n\
                     \x20           'rating' => 'integer|min:-1|max:1',\n        ];\n    }\n}\n",
                ),
            ],
        );
        assert_eq!(
            source.routes,
            vec![(
                "/v1/blocks".to_string(),
                "post",
                Some("StoreBlockRequest".into())
            )]
        );
        let fields = source.bodies.get("StoreBlockRequest").expect("resolved");
        assert_eq!(
            fields["blocked_type"].allowed.as_deref(),
            Some(["user".to_string(), "sponsor".to_string()].as_slice())
        );
        assert!(fields["blocked_type"].required);
        assert_eq!(fields["rating"].range, Some((Some(-1.0), Some(1.0))));
        assert!(!fields["rating"].required);
    }

    #[test]
    fn the_array_rule_form_reads_the_same_as_the_pipe_form() {
        let source = read_source(
            "arrayform",
            &[
                ("routes.php", ROUTES),
                (
                    "StoreBlockRequest.php",
                    "<?php\nclass StoreBlockRequest\n{\n    public function rules()\n    {\n\
                     \x20       return [\n            'blocked_type' => ['required', 'in:user,sponsor'],\n\
                     \x20       ];\n    }\n}\n",
                ),
            ],
        );
        let fields = source.bodies.get("StoreBlockRequest").expect("resolved");
        assert!(fields["blocked_type"].required);
        assert_eq!(
            fields["blocked_type"].allowed.as_deref(),
            Some(["user".to_string(), "sponsor".to_string()].as_slice())
        );
    }

    #[test]
    fn the_legacy_string_action_form_names_its_controller() {
        let source = read_source(
            "legacy",
            &[(
                "routes.php",
                "<?php\nRoute::get('/healthz', 'HealthController@index');\n",
            )],
        );
        assert_eq!(
            source.routes,
            vec![(
                "/healthz".to_string(),
                "get",
                Some("HealthController".into())
            )]
        );
    }

    #[test]
    fn between_states_both_bounds() {
        let source = read_source(
            "between",
            &[
                ("routes.php", ROUTES),
                (
                    "StoreBlockRequest.php",
                    "<?php\nclass StoreBlockRequest\n{\n    public function rules()\n    {\n\
                     \x20       return ['rating' => 'required|between:1,5'];\n    }\n}\n",
                ),
            ],
        );
        let fields = source.bodies.get("StoreBlockRequest").expect("resolved");
        assert_eq!(fields["rating"].range, Some((Some(1.0), Some(5.0))));
    }

    #[test]
    fn a_nested_rule_key_is_skipped_rather_than_flattened() {
        let source = read_source(
            "nested",
            &[
                ("routes.php", ROUTES),
                (
                    "StoreBlockRequest.php",
                    "<?php\nclass StoreBlockRequest\n{\n    public function rules()\n    {\n\
                     \x20       return ['user.name' => 'required', 'ok' => 'required'];\n    }\n}\n",
                ),
            ],
        );
        let fields = source.bodies.get("StoreBlockRequest").expect("resolved");
        assert!(fields.contains_key("ok"));
        assert!(
            !fields.contains_key("user") && !fields.contains_key("user.name"),
            "a sub-object rule must not become a top-level fact: {fields:?}"
        );
    }

    #[test]
    fn two_classes_of_the_same_name_resolve_to_neither() {
        let source = read_source(
            "ambiguous",
            &[
                ("routes.php", ROUTES),
                (
                    "a.php",
                    "<?php\nclass StoreBlockRequest\n{\n    public function rules()\n\
                     \x20   { return ['a' => 'required']; }\n}\n",
                ),
                (
                    "b.php",
                    "<?php\nclass StoreBlockRequest\n{\n    public function rules()\n\
                     \x20   { return ['b' => 'required']; }\n}\n",
                ),
            ],
        );
        assert!(
            !source.bodies.contains_key("StoreBlockRequest"),
            "an ambiguous class must abstain: {:?}",
            source.bodies
        );
    }

    #[test]
    fn a_path_without_a_leading_slash_is_still_a_route() {
        // Requiring one cost a real application 326 of its 328 routes.
        let source = read_source(
            "slashless",
            &[(
                "routes.php",
                "<?php\nRoute::get('noslash', fn() => 1);\n\
                 Route::apiResource('users', UserController::class);\n\
                 Route::prefix('v1')->group(function () {\n\
                 \x20   Route::get('/inner', fn() => 1);\n});\n\
                 Route::group(['prefix' => 'v2'], function () {\n\
                 \x20   Route::get('/other', fn() => 1);\n});\n",
            )],
        );
        let paths: Vec<&String> = source.routes.iter().map(|(path, _, _)| path).collect();
        assert!(paths.contains(&&"/noslash".to_string()), "{paths:?}");
        assert!(
            paths.contains(&&"/users".to_string()),
            "apiResource: {paths:?}"
        );
        assert!(paths.contains(&&"/v1/inner".to_string()), "{paths:?}");
        assert!(paths.contains(&&"/v2/other".to_string()), "{paths:?}");
    }

    #[test]
    fn a_file_that_does_not_parse_is_counted() {
        let source = read_source(
            "broken",
            &[
                ("ok.php", "<?php\n$x = 1;\n"),
                ("bad.php", "<?php\nfunction f( {\n"),
            ],
        );
        assert_eq!(source.files_parsed, 1);
        assert_eq!(source.files_unreadable, 1);
    }

    #[test]
    fn a_non_route_call_with_a_string_is_not_a_route() {
        let source = read_source(
            "notaroute",
            &[("app.php", "<?php\nLog::info('/not/a/route');\n")],
        );
        assert!(source.routes.is_empty(), "{:?}", source.routes);
    }
}
