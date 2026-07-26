//! Go route and body extraction over the grammar.
//!
//! Go states its constraints in struct tags, which the pattern reader already
//! read well. What it could not do is connect them: `var body BlockRequest`
//! followed by `c.ShouldBindJSON(&body)` names the handler's request type
//! through a local, and matching that across lines with a regex meant guessing.
//! Over a parse the local's declaration and its use are the same scope.
//!
//! Router groups compose the same way. `v1 := r.Group("/v1")` then
//! `v1.POST("/blocks", h)` is a prefix travelling with a variable, which is
//! only sound to follow when the binding is visible.

use super::extract::Family;
use super::field_facts::{drop_ambiguous, record, FieldFact};
use super::grammar::{self, SourceRead, MAX_FIELDS};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use tree_sitter::Node;

const METHODS: [&str; 7] = ["get", "post", "put", "patch", "delete", "head", "options"];
/// Bound how far a chain of `Group` calls may nest.
const MAX_GROUP_DEPTH: usize = 8;

/// Router variable -> (the router it was grouped off, its own prefix segment).
type Groups = BTreeMap<String, (String, String)>;

pub(super) fn read(root: &Path) -> SourceRead {
    let mut source = SourceRead::default();
    let mut structs: BTreeMap<String, BTreeMap<String, FieldFact>> = BTreeMap::new();
    let mut ambiguous: BTreeSet<String> = BTreeSet::new();
    // handler fn -> the struct it binds its request body into.
    let mut handler_body: BTreeMap<String, String> = BTreeMap::new();
    // router variable -> (the router it was grouped off, its own prefix).
    let mut groups: Groups = BTreeMap::new();
    let mut raw: Vec<(String, String, &'static str, Option<String>)> = Vec::new();

    grammar::read_files(
        root,
        Family::Go,
        tree_sitter_go::LANGUAGE.into(),
        &mut source,
        |root_node, text| {
            grammar::walk(root_node, &mut |node| match node.kind() {
                "type_spec" => collect_struct(node, text, &mut structs, &mut ambiguous),
                "function_declaration" => collect_handler(node, text, &mut handler_body),
                "short_var_declaration" => collect_group(node, text, &mut groups),
                "call_expression" => collect_route(node, text, &mut raw),
                _ => {}
            });
        },
    );
    drop_ambiguous(&mut structs, &ambiguous);

    for (router, path, method, handler) in raw {
        let path = match resolve_prefix(&groups, &router) {
            Some(prefix) => format!("{}{path}", prefix.trim_end_matches('/')),
            None => path,
        };
        source.routes.push((path, method, handler.clone()));
        if let Some(handler) = handler {
            if let Some(fields) = handler_body
                .get(&handler)
                .and_then(|name| structs.get(name))
                .cloned()
            {
                source.bodies.insert(handler, fields);
            }
        }
    }
    source
}

/// A group's full prefix, composing outward through `admin := v1.Group("/admin")`.
///
/// Bounded and cycle-guarded: Go cannot actually write a cyclic group chain,
/// but this reads whatever is on disk, and a reader that can loop on malformed
/// input is a reader that hangs a CI job.
fn resolve_prefix(groups: &Groups, router: &str) -> Option<String> {
    let (mut parent, own) = groups.get(router)?.clone();
    let mut prefix = own;
    let mut seen = BTreeSet::from([router.to_string()]);
    for _ in 0..MAX_GROUP_DEPTH {
        if !seen.insert(parent.clone()) {
            break;
        }
        let Some((outer, own)) = groups.get(&parent) else {
            break;
        };
        prefix = format!("{}{prefix}", own.trim_end_matches('/'));
        parent = outer.clone();
    }
    Some(prefix)
}

/// `type BlockRequest struct { ... }` with its json/binding tags.
fn collect_struct(
    node: Node,
    text: &str,
    structs: &mut BTreeMap<String, BTreeMap<String, FieldFact>>,
    ambiguous: &mut BTreeSet<String>,
) {
    let Some(name) = grammar::field(node, text, "name") else {
        return;
    };
    let Some(body) = node
        .child_by_field_name("type")
        .filter(|ty| ty.kind() == "struct_type")
        .and_then(|ty| grammar::find(ty, "field_declaration_list"))
    else {
        return;
    };
    let mut fields = BTreeMap::new();
    let mut declarations = Vec::new();
    grammar::children(body, &mut declarations);
    for declaration in declarations.into_iter().take(MAX_FIELDS) {
        if declaration.kind() != "field_declaration" {
            continue;
        }
        let Some(tag) = declaration
            .child_by_field_name("tag")
            .map(|tag| grammar::text(tag, text).trim_matches('`').to_string())
        else {
            continue;
        };
        let Some(json) = tag_value(&tag, "json") else {
            continue;
        };
        let json = json.split(',').next().unwrap_or(&json).to_string();
        if json.is_empty() || json == "-" {
            continue;
        }
        let rules = tag_value(&tag, "binding")
            .or_else(|| tag_value(&tag, "validate"))
            .unwrap_or_default();
        fields.insert(json, fact(&rules));
    }
    if !fields.is_empty() {
        record(structs, ambiguous, name, fields);
    }
}

/// One key of a struct tag: `json:"blocked_id" binding:"required"`.
fn tag_value(tag: &str, key: &str) -> Option<String> {
    let at = tag.find(&format!("{key}:\""))? + key.len() + 2;
    let rest = &tag[at..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// A `binding` / `validate` rule list: `required,oneof=user sponsor,min=-1`.
fn fact(rules: &str) -> FieldFact {
    let mut allowed = None;
    let mut low = None;
    let mut high = None;
    let mut required = false;
    for rule in rules.split(',') {
        let rule = rule.trim();
        if rule == "required" {
            required = true;
        } else if let Some(values) = rule.strip_prefix("oneof=") {
            let values: Vec<String> = values.split_whitespace().map(str::to_string).collect();
            if values.len() > 1 {
                allowed = Some(values);
            }
        } else if let Some(value) = rule.strip_prefix("min=") {
            low = grammar::number(value);
        } else if let Some(value) = rule.strip_prefix("max=") {
            high = grammar::number(value);
        } else if let Some(value) = rule.strip_prefix("gte=") {
            low = grammar::number(value);
        } else if let Some(value) = rule.strip_prefix("lte=") {
            high = grammar::number(value);
        }
    }
    // `min=-1,max=1` on the SAME rule list is one range, and the grammar splits
    // the rules but not the pairing, so the two bounds compose here.
    let range = (low.is_some() || high.is_some()).then_some((low, high));
    FieldFact {
        required,
        evidence: match (&allowed, &range) {
            (Some(_), _) => Some("a struct tag `oneof` rule".to_string()),
            (_, Some(_)) => Some("a struct tag min/max rule".to_string()),
            _ => None,
        },
        allowed,
        range,
    }
}

/// `func createBlock(c *gin.Context) { var body BlockRequest; c.ShouldBindJSON(&body) }`
///
/// The bind call names a LOCAL, so the request type is whatever that local was
/// declared as. Reading the declaration is the whole reason to be on a parse.
fn collect_handler(node: Node, text: &str, handler_body: &mut BTreeMap<String, String>) {
    let Some(name) = grammar::field(node, text, "name") else {
        return;
    };
    let Some(body) = node.child_by_field_name("body") else {
        return;
    };
    let mut locals: BTreeMap<String, String> = BTreeMap::new();
    let mut bound: Option<String> = None;
    grammar::walk(body, &mut |inner| match inner.kind() {
        "var_spec" => {
            if let (Some(local), Some(ty)) = (
                grammar::field(inner, text, "name"),
                grammar::field(inner, text, "type"),
            ) {
                locals.insert(local, ty);
            }
        }
        "call_expression" => {
            let callee = inner
                .child_by_field_name("function")
                .and_then(|f| grammar::field(f, text, "field"))
                .unwrap_or_default();
            if !matches!(
                callee.as_str(),
                "ShouldBindJSON" | "BindJSON" | "Bind" | "ShouldBind" | "BodyParser" | "Decode"
            ) {
                return;
            }
            if let Some(arguments) = inner.child_by_field_name("arguments") {
                let mut args = Vec::new();
                grammar::children(arguments, &mut args);
                if let Some(first) = args.first() {
                    let named = grammar::text(*first, text)
                        .trim_start_matches('&')
                        .to_string();
                    // A composite literal binds its own type: `&BlockRequest{}`.
                    let ty = locals
                        .get(&named)
                        .cloned()
                        .or_else(|| named.split('{').next().map(str::to_string));
                    bound = ty.filter(|ty| !ty.is_empty());
                }
            }
        }
        _ => {}
    });
    if let Some(ty) = bound {
        handler_body.insert(name, ty);
    }
}

/// `v1 := r.Group("/v1")`. The parent router is recorded with the prefix so a
/// nested group composes rather than dropping its outer segment.
fn collect_group(node: Node, text: &str, groups: &mut Groups) {
    let mut left = Vec::new();
    let mut right = Vec::new();
    if let Some(node) = node.child_by_field_name("left") {
        grammar::children(node, &mut left);
    }
    if let Some(node) = node.child_by_field_name("right") {
        grammar::children(node, &mut right);
    }
    let (Some(name), Some(value)) = (left.first(), right.first()) else {
        return;
    };
    if value.kind() != "call_expression" {
        return;
    }
    let Some(function) = value.child_by_field_name("function") else {
        return;
    };
    if grammar::field(function, text, "field").as_deref() != Some("Group") {
        return;
    }
    let parent = grammar::field(function, text, "operand").unwrap_or_default();
    let Some(arguments) = value.child_by_field_name("arguments") else {
        return;
    };
    let mut args = Vec::new();
    grammar::children(arguments, &mut args);
    let Some(prefix) = args
        .first()
        .map(|node| grammar::unquote(grammar::text(*node, text)).to_string())
        .filter(|prefix| prefix.starts_with('/'))
    else {
        return;
    };
    groups.insert(grammar::text(*name, text).to_string(), (parent, prefix));
}

/// `v1.POST("/blocks", createBlock)` and `mux.HandleFunc("GET /healthz", h)`.
fn collect_route(
    node: Node,
    text: &str,
    raw: &mut Vec<(String, String, &'static str, Option<String>)>,
) {
    let Some(function) = node.child_by_field_name("function") else {
        return;
    };
    if function.kind() != "selector_expression" {
        return;
    }
    let router = grammar::field(function, text, "operand").unwrap_or_default();
    let called = grammar::field(function, text, "field").unwrap_or_default();
    let Some(arguments) = node.child_by_field_name("arguments") else {
        return;
    };
    let mut args = Vec::new();
    grammar::children(arguments, &mut args);
    let Some(first) = args.first() else {
        return;
    };
    let literal = grammar::unquote(grammar::text(*first, text)).to_string();
    let handler = args
        .get(1)
        .map(|node| grammar::text(*node, text))
        .and_then(last_segment);

    // Go 1.22 net/http puts the verb inside the pattern: `"GET /healthz"`.
    if called == "HandleFunc" || called == "Handle" {
        if let Some((verb, path)) = literal.split_once(' ') {
            if let Some(method) = method_of(verb) {
                raw.push((String::new(), path.to_string(), method, handler));
            }
        } else if literal.starts_with('/') {
            // No verb stated: net/http serves every method on this pattern, and
            // claiming one would be an invention. GET is the only one a derived
            // draft can exercise safely.
            raw.push((String::new(), literal, "get", handler));
        }
        return;
    }
    if let (Some(method), true) = (method_of(&called), literal.starts_with('/')) {
        raw.push((router, literal, method, handler));
    }
}

/// `handlers.CreateBlock` -> `CreateBlock`, so a handler named through its
/// package resolves to the same key the declaration was recorded under.
fn last_segment(raw: &str) -> Option<String> {
    let name = raw.rsplit('.').next()?.trim();
    let valid = !name.is_empty()
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !name.starts_with(|c: char| c.is_ascii_digit());
    valid.then(|| name.to_string())
}

fn method_of(verb: &str) -> Option<&'static str> {
    let lower = verb.to_ascii_lowercase();
    METHODS.into_iter().find(|known| *known == lower)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_source(case: &str, files: &[(&str, &str)]) -> SourceRead {
        let root =
            std::env::temp_dir().join(format!("reproit-goast-{}-{case}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("root");
        for (name, body) in files {
            std::fs::write(root.join(name), body).expect("write");
        }
        let source = read(&root);
        let _ = std::fs::remove_dir_all(&root);
        source
    }

    const SERVICE: &str = r#"package main

type BlockRequest struct {
	BlockedID   string  `json:"blocked_id" binding:"required"`
	BlockedType string  `json:"blocked_type" binding:"required,oneof=user sponsor"`
	Rating      int     `json:"rating" binding:"required,min=-1,max=1"`
	Note        *string `json:"note"`
}

func createBlock(c *gin.Context) {
	var body BlockRequest
	if err := c.ShouldBindJSON(&body); err != nil {
		return
	}
}

func main() {
	r := gin.Default()
	v1 := r.Group("/v1")
	v1.POST("/blocks", createBlock)
}
"#;

    #[test]
    fn a_group_prefix_travels_with_its_variable() {
        let source = read_source("group", &[("main.go", SERVICE)]);
        assert_eq!(
            source.routes,
            vec![("/v1/blocks".to_string(), "post", Some("createBlock".into()))]
        );
    }

    #[test]
    fn a_body_bound_through_a_local_resolves_to_its_struct() {
        let source = read_source("bind", &[("main.go", SERVICE)]);
        let fields = source.bodies.get("createBlock").expect("resolved");
        assert_eq!(
            fields["blocked_type"].allowed.as_deref(),
            Some(["user".to_string(), "sponsor".to_string()].as_slice())
        );
        assert_eq!(fields["rating"].range, Some((Some(-1.0), Some(1.0))));
        assert!(fields["blocked_id"].required);
        assert!(!fields["note"].required, "no binding rule is not required");
    }

    #[test]
    fn nested_groups_compose_their_prefixes() {
        let source = read_source(
            "nested",
            &[(
                "main.go",
                "package main\nfunc main() {\n\tr := gin.Default()\n\tv1 := r.Group(\"/v1\")\n\
                 \tadmin := v1.Group(\"/admin\")\n\tadmin.GET(\"/users\", listUsers)\n}\n",
            )],
        );
        assert_eq!(
            source.routes,
            vec![(
                "/v1/admin/users".to_string(),
                "get",
                Some("listUsers".into())
            )]
        );
    }

    #[test]
    fn a_method_prefixed_net_http_pattern_keeps_its_verb() {
        let source = read_source(
            "nethttp",
            &[(
                "main.go",
                "package main\nfunc main() {\n\tmux.HandleFunc(\"GET /healthz\", health)\n}\n",
            )],
        );
        assert_eq!(
            source.routes,
            vec![("/healthz".to_string(), "get", Some("health".into()))]
        );
    }

    #[test]
    fn a_handler_named_through_its_package_resolves_to_the_bare_name() {
        let source = read_source(
            "qualified",
            &[(
                "main.go",
                "package main\nfunc main() {\n\tr.POST(\"/x\", handlers.CreateBlock)\n}\n",
            )],
        );
        assert_eq!(
            source.routes,
            vec![("/x".to_string(), "post", Some("CreateBlock".into()))]
        );
    }

    #[test]
    fn a_file_that_does_not_parse_is_counted() {
        let source = read_source(
            "broken",
            &[
                ("ok.go", "package main\n\nfunc main() {}\n"),
                ("bad.go", "package main\n\nfunc main( {\n"),
            ],
        );
        assert_eq!(source.files_parsed, 1);
        assert_eq!(source.files_unreadable, 1);
    }

    #[test]
    fn two_structs_of_the_same_name_resolve_to_neither() {
        let source = read_source(
            "ambiguous",
            &[
                (
                    "a.go",
                    "package a\ntype Req struct {\n\tA string `json:\"a\" binding:\"required\"`\n}\n\
                     func h(c *gin.Context) {\n\tvar body Req\n\tc.ShouldBindJSON(&body)\n}\n\
                     func main() { r.POST(\"/x\", h) }\n",
                ),
                (
                    "b.go",
                    "package b\ntype Req struct {\n\tB string `json:\"b\" binding:\"required\"`\n}\n",
                ),
            ],
        );
        assert!(
            !source.bodies.contains_key("h"),
            "an ambiguous type must abstain: {:?}",
            source.bodies
        );
    }

    #[test]
    fn a_non_route_call_with_a_string_is_not_a_route() {
        let source = read_source(
            "notaroute",
            &[(
                "main.go",
                "package main\nfunc main() {\n\tlog.Print(\"/not/a/route\")\n}\n",
            )],
        );
        assert!(source.routes.is_empty(), "{:?}", source.routes);
    }
}
