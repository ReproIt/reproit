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
use super::field_facts::drop_ambiguous;
use super::go_types;
use super::grammar::{self, SourceRead};
use super::response_facts::ResponseFact;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use tree_sitter::Node;

const METHODS: [&str; 7] = ["get", "post", "put", "patch", "delete", "head", "options"];
/// Bound how far a chain of `Group` calls may nest.
const MAX_GROUP_DEPTH: usize = 8;

/// Router variable -> (the router it was grouped off, its own prefix segment).
type Groups = BTreeMap<String, (String, String)>;
/// Router builder identity -> every literal prefix where it is mounted.
type Mounts = BTreeMap<String, BTreeSet<String>>;

/// Marks a group whose prefix could not be read. Its routes are real but their
/// location is not knowable from source, and emitting them at the root would
/// name paths the service does not serve.
const OPAQUE: &str = "\u{1}opaque";

pub(super) fn read(root: &Path) -> SourceRead {
    let mut source = SourceRead::default();
    let mut structs = go_types::Structs::default();
    // handler fn -> the struct it binds its request body into.
    let mut handler_body: BTreeMap<String, String> = BTreeMap::new();
    // handler fn -> the response statuses and bodies its code states.
    let mut handler_responses: BTreeMap<String, ResponseFact> = BTreeMap::new();
    // Router builders and mount sites are collected independently, then joined
    // after every file is read. chi commonly declares a receiver method in one
    // file and mounts it from main.go.
    let mut built: Vec<(String, String, &'static str, Option<String>)> = Vec::new();
    let mut mounts = Mounts::new();

    grammar::read_files(
        root,
        Family::Go,
        tree_sitter_go::LANGUAGE.into(),
        &mut source,
        |root_node, text, path| {
            let file_vars = go_types::package_vars(root_node, text);
            grammar::walk(root_node, &mut |node| match node.kind() {
                "type_spec" => go_types::collect_struct(node, text, &mut structs),
                "function_declaration" | "method_declaration" => go_types::collect_handler(
                    node,
                    text,
                    &file_vars,
                    &mut handler_body,
                    &mut handler_responses,
                ),
                _ => {}
            });

            collect_mounts_under(root, path, root_node, text, "", &mut mounts);

            let mut declarations = Vec::new();
            grammar::children(root_node, &mut declarations);
            let mut found_declaration = false;
            for declaration in declarations {
                if !matches!(
                    declaration.kind(),
                    "function_declaration" | "method_declaration"
                ) {
                    continue;
                }
                found_declaration = true;
                let Some(builder) = declaration_key(root, path, declaration, text) else {
                    continue;
                };
                let mut groups = Groups::new();
                grammar::walk(declaration, &mut |node| {
                    if node.kind() == "short_var_declaration" {
                        collect_group(node, text, &mut groups);
                    }
                });
                let mut found = Vec::new();
                routes_under(declaration, text, "", &mut found);
                for (router, path, method, handler) in found {
                    let path = match resolve_prefix(&groups, &router) {
                        Some(prefix) if prefix.contains(OPAQUE) => continue,
                        Some(prefix) => format!("{}{path}", prefix.trim_end_matches('/')),
                        None => path,
                    };
                    let path = if path.is_empty() {
                        "/".to_string()
                    } else {
                        path
                    };
                    built.push((builder.clone(), path, method, handler));
                }
            }
            // Preserve the reader's long-standing snippet contract. Real Go
            // programs take the declaration path above; this fallback exists
            // only for a source fragment with calls directly at the root.
            if !found_declaration {
                let mut groups = Groups::new();
                grammar::walk(root_node, &mut |node| {
                    if node.kind() == "short_var_declaration" {
                        collect_group(node, text, &mut groups);
                    }
                });
                let mut found = Vec::new();
                routes_under(root_node, text, "", &mut found);
                let builder = scoped_builder_key(root, path, "<root>");
                for (router, path, method, handler) in found {
                    let path = resolve_prefix(&groups, &router)
                        .map(|prefix| format!("{}{path}", prefix.trim_end_matches('/')))
                        .unwrap_or(path);
                    if !path.contains(OPAQUE) {
                        built.push((builder.clone(), path, method, handler));
                    }
                }
            }
        },
    );
    drop_ambiguous(&mut structs.facts, &structs.facts_ambiguous);
    drop_ambiguous(&mut structs.wire, &structs.wire_ambiguous);

    for (builder, path, method, handler) in built {
        let prefixes = mounts
            .get(&builder)
            .map(|prefixes| prefixes.iter().map(String::as_str).collect())
            .unwrap_or_else(|| vec![""]);
        for prefix in prefixes {
            if prefix.contains(OPAQUE) {
                continue;
            }
            let mounted_path = format!("{}{path}", prefix.trim_end_matches('/'));
            source.routes.push((mounted_path, method, handler.clone()));
            if let Some(handler) = &handler {
                if let Some(fields) = handler_body
                    .get(handler)
                    .and_then(|name| structs.facts.get(name))
                    .cloned()
                {
                    source.bodies.insert(handler.clone(), fields);
                }
                if let Some(fact) = handler_responses.get(handler) {
                    source.responses.insert(handler.clone(), fact.clone());
                }
            }
        }
    }
    source.serializers = structs.wire;
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

/// `v1 := r.Group("/v1")`. The parent router is recorded with the prefix so a
/// nested group composes rather than dropping its outer segment.
/// The `X.Group("...")` call inside an expression, past anything chained onto
/// it.
fn innermost_group<'a>(node: Node<'a>, text: &str) -> Option<Node<'a>> {
    if node.kind() == "call_expression" {
        if let Some(callee) = node.child_by_field_name("function") {
            if grammar::field(callee, text, "field").as_deref() == Some("Group") {
                return Some(node);
            }
        }
    }
    let mut cursor = node.walk();
    let children: Vec<Node<'a>> = node.children(&mut cursor).collect();
    children
        .into_iter()
        .find_map(|child| innermost_group(child, text))
}

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
    // `app.Group("/todo").Use(mw)` is a Group call with more chained onto it.
    // Only matching the outermost call missed it, and the routes hung off that
    // variable were then emitted at the root: four paths nothing serves.
    let Some(group) = innermost_group(*value, text) else {
        return;
    };
    let function = group.child_by_field_name("function").unwrap_or(function);
    let parent = grammar::field(function, text, "operand").unwrap_or_default();
    let value = &group;
    let Some(arguments) = value.child_by_field_name("arguments") else {
        return;
    };
    let mut args = Vec::new();
    grammar::children(arguments, &mut args);
    let Some(first) = args.first() else {
        return;
    };
    // A group prefix built from a constant or variable is unknowable here.
    // Dropping it silently emitted the inner routes at the root, which are
    // paths the service does not serve, so the group is recorded as OPAQUE and
    // its routes are skipped instead.
    let literal = matches!(
        first.kind(),
        "interpreted_string_literal" | "raw_string_literal"
    );
    let prefix = grammar::unquote(grammar::text(*first, text)).to_string();
    if !literal || !prefix.starts_with('/') {
        groups.insert(
            grammar::text(*name, text).to_string(),
            (String::new(), OPAQUE.to_string()),
        );
        return;
    }
    groups.insert(grammar::text(*name, text).to_string(), (parent, prefix));
}

/// Routes under a lexical prefix, descending into the closures that extend it.
fn routes_under(
    node: Node,
    text: &str,
    prefix: &str,
    out: &mut Vec<(String, String, &'static str, Option<String>)>,
) {
    let mut children = Vec::new();
    grammar::children(node, &mut children);
    for child in children {
        if let Some((inner, body)) = nested_router(child, text, prefix) {
            routes_under(body, text, &inner, out);
            continue;
        }
        if child.kind() == "call_expression" {
            let mut here = Vec::new();
            collect_route(child, text, &mut here);
            for (router, path, method, handler) in here {
                out.push((router, format!("{prefix}{path}"), method, handler));
            }
        }
        routes_under(child, text, prefix, out);
    }
}

/// Mount calls under a lexical chi prefix.
fn collect_mounts_under(
    root: &Path,
    path: &Path,
    node: Node,
    text: &str,
    prefix: &str,
    mounts: &mut Mounts,
) {
    let mut children = Vec::new();
    grammar::children(node, &mut children);
    for child in children {
        if let Some((inner, body)) = nested_router(child, text, prefix) {
            collect_mounts_under(root, path, body, text, &inner, mounts);
            continue;
        }
        collect_mount(root, path, child, text, prefix, mounts);
        collect_mounts_under(root, path, child, text, prefix, mounts);
    }
}

/// `r.Mount("/admin", adminRouter())` -> the builder and full mount prefix.
fn collect_mount(
    root: &Path,
    path: &Path,
    node: Node,
    text: &str,
    outer: &str,
    mounts: &mut Mounts,
) {
    if node.kind() != "call_expression" {
        return;
    }
    let Some(callee) = node.child_by_field_name("function") else {
        return;
    };
    if grammar::field(callee, text, "field").as_deref() != Some("Mount") {
        return;
    }
    let Some(arguments) = node.child_by_field_name("arguments") else {
        return;
    };
    let mut args = Vec::new();
    grammar::children(arguments, &mut args);
    let (Some(first), Some(second)) = (args.first(), args.get(1)) else {
        return;
    };
    let literal = matches!(
        first.kind(),
        "interpreted_string_literal" | "raw_string_literal"
    );
    let own = grammar::unquote(grammar::text(*first, text));
    let prefix = if literal && own.starts_with('/') {
        format!(
            "{}{}",
            outer.trim_end_matches('/'),
            own.trim_end_matches('/')
        )
    } else {
        OPAQUE.to_string()
    };
    let Some(target) = mount_target(*second, text) else {
        return;
    };
    mounts
        .entry(scoped_builder_key(root, path, &target))
        .or_default()
        .insert(prefix);
}

/// The local builder invoked as a mount target.
///
/// Plain functions use their function name. Receiver methods use the concrete
/// composite-literal type plus method name, so `usersResource{}.Routes()` and
/// `todosResource{}.Routes()` remain distinct.
fn mount_target(node: Node, text: &str) -> Option<String> {
    let call = (node.kind() == "call_expression").then_some(node)?;
    let function = call.child_by_field_name("function")?;
    match function.kind() {
        "identifier" => Some(grammar::text(function, text).to_string()),
        "selector_expression" => {
            let method = grammar::field(function, text, "field")?;
            let receiver = function.child_by_field_name("operand")?;
            let receiver_type = grammar::find(receiver, "type_identifier")
                .map(|node| grammar::text(node, text).to_string())?;
            Some(format!("{receiver_type}.{method}"))
        }
        _ => None,
    }
}

/// Stable identity for a function or receiver method that builds routes.
fn declaration_key(root: &Path, path: &Path, node: Node, text: &str) -> Option<String> {
    let name = grammar::field(node, text, "name")?;
    let symbol = if node.kind() == "method_declaration" {
        let receiver = node.child_by_field_name("receiver")?;
        let receiver_type = grammar::find(receiver, "type_identifier")
            .map(|node| grammar::text(node, text).to_string())?;
        format!("{receiver_type}.{name}")
    } else {
        name
    };
    Some(scoped_builder_key(root, path, &symbol))
}

/// Go declarations can only be joined across files in the same package
/// directory. Including that directory prevents same-named builders in sibling
/// packages from borrowing each other's mount prefixes.
fn scoped_builder_key(root: &Path, path: &Path, symbol: &str) -> String {
    let directory = path
        .parent()
        .and_then(|parent| parent.strip_prefix(root).ok())
        .unwrap_or_else(|| Path::new(""));
    format!("{}:{symbol}", directory.display())
}

/// A chi `Route("/x", func(...))` or `Mount("/x", handler())` and the prefix
/// its body inherits.
fn nested_router<'a>(node: Node<'a>, text: &str, outer: &str) -> Option<(String, Node<'a>)> {
    if node.kind() != "call_expression" {
        return None;
    }
    let callee = node.child_by_field_name("function")?;
    let called = grammar::field(callee, text, "field")?;
    if called != "Route" && called != "Mount" && called != "Group" {
        return None;
    }
    let arguments = node.child_by_field_name("arguments")?;
    let mut args = Vec::new();
    grammar::children(arguments, &mut args);
    let first = args.first()?;
    if !matches!(
        first.kind(),
        "interpreted_string_literal" | "raw_string_literal"
    ) {
        return None;
    }
    let prefix = grammar::unquote(grammar::text(*first, text));
    // A `Group("/v1")` with no closure is the variable form, handled elsewhere.
    let body = args.iter().find(|arg| arg.kind() == "func_literal")?;
    Some((
        format!(
            "{}{}",
            outer.trim_end_matches('/'),
            prefix.trim_end_matches('/')
        ),
        *body,
    ))
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
    // Only a string LITERAL is a path. `r.Group(adminBase)` names a constant
    // this cannot resolve; treating its text as a path emitted a route nobody
    // serves.
    if !matches!(
        first.kind(),
        "interpreted_string_literal" | "raw_string_literal"
    ) {
        return;
    }
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
    // `u.GET("", h)` registers the group's own collection route. Requiring a
    // leading slash dropped every collection endpoint in a grouped service.
    if let Some(method) = method_of(&called) {
        if literal.is_empty() || literal.starts_with('/') {
            raw.push((router, literal, method, handler));
        }
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
    fn a_gin_handler_states_its_response_statuses_and_typed_bodies() {
        use super::super::response_facts::WireShape;
        let source = read_source(
            "gin_responses",
            &[(
                "main.go",
                "package main\n\
                 type Item struct {\n\
                 \tID    string  `json:\"id\"`\n\
                 \tPrice float64 `json:\"price\"`\n\
                 \tNote  *string `json:\"note,omitempty\"`\n\
                 \tcost  int\n\
                 }\n\
                 func listItems(c *gin.Context) {\n\
                 \titems := []Item{}\n\
                 \tc.JSON(http.StatusOK, items)\n\
                 }\n\
                 func createItem(c *gin.Context) {\n\
                 \tvar body Item\n\
                 \tif err := c.ShouldBindJSON(&body); err != nil {\n\
                 \t\tc.JSON(400, gin.H{\"error\": \"bad\"})\n\
                 \t\treturn\n\
                 \t}\n\
                 \tc.JSON(http.StatusCreated, body)\n\
                 }\n\
                 func main() {\n\
                 \tr := gin.Default()\n\
                 \tr.GET(\"/items\", listItems)\n\
                 \tr.POST(\"/items\", createItem)\n\
                 }\n",
            )],
        );
        let list = source.responses.get("listItems").expect("stated");
        assert_eq!(
            list.statuses[&200],
            WireShape::Array(Box::new(WireShape::Named("Item".into())))
        );
        let create = source.responses.get("createItem").expect("stated");
        assert_eq!(create.statuses[&201], WireShape::Named("Item".into()));
        assert_eq!(create.statuses[&400], WireShape::Object, "gin.H is a map");
        let item = source.serializers.get("Item").expect("collected");
        assert_eq!(item["id"].shape, WireShape::Primitive("string"));
        assert!(item["id"].required);
        assert_eq!(item["price"].shape, WireShape::Primitive("number"));
        assert!(!item["note"].required, "omitempty is not required");
        assert_eq!(
            item["note"].shape,
            WireShape::Primitive("string"),
            "an omitempty pointer states its pointee's type when present"
        );
        assert!(
            !item.contains_key("cost"),
            "an unexported field never serializes: {:?}",
            item.keys()
        );
    }

    #[test]
    fn a_net_http_handler_pairs_write_header_with_encode() {
        use super::super::response_facts::WireShape;
        let source = read_source(
            "nethttp_responses",
            &[(
                "main.go",
                "package main\n\
                 type Health struct {\n\tOK bool `json:\"ok\"`\n}\n\
                 var all []Health\n\
                 func health(w http.ResponseWriter, r *http.Request) {\n\
                 \tjson.NewEncoder(w).Encode(Health{OK: true})\n\
                 }\n\
                 func listAll(w http.ResponseWriter, r *http.Request) {\n\
                 \tjson.NewEncoder(w).Encode(all)\n\
                 }\n\
                 func missing(w http.ResponseWriter, r *http.Request) {\n\
                 \thttp.Error(w, \"gone\", http.StatusNotFound)\n\
                 }\n\
                 func created(w http.ResponseWriter, r *http.Request) {\n\
                 \tw.WriteHeader(http.StatusCreated)\n\
                 \tjson.NewEncoder(w).Encode(Health{OK: true})\n\
                 }\n\
                 func main() {\n\
                 \tmux.HandleFunc(\"GET /healthz\", health)\n\
                 \tmux.HandleFunc(\"GET /missing\", missing)\n\
                 \tmux.HandleFunc(\"POST /created\", created)\n\
                 \tmux.HandleFunc(\"GET /all\", listAll)\n\
                 }\n",
            )],
        );
        let health = source.responses.get("health").expect("stated");
        assert_eq!(
            health.statuses[&200],
            WireShape::Named("Health".into()),
            "an Encode with no WriteHeader is net/http's implicit 200"
        );
        let listed = source.responses.get("listAll").expect("stated");
        assert_eq!(
            listed.statuses[&200],
            WireShape::Array(Box::new(WireShape::Named("Health".into()))),
            "a package-level var's type reaches the handler that serves it"
        );
        let missing = source.responses.get("missing").expect("stated");
        assert_eq!(missing.statuses[&404], WireShape::Unknown);
        let created = source.responses.get("created").expect("stated");
        assert_eq!(created.statuses[&201], WireShape::Named("Health".into()));
        assert!(
            !created.statuses.contains_key(&200),
            "the pending WriteHeader status claims the Encode: {:?}",
            created.statuses
        );
    }

    #[test]
    fn an_unreadable_status_or_body_states_nothing_rather_than_a_guess() {
        use super::super::response_facts::WireShape;
        let source = read_source(
            "opaque_responses",
            &[(
                "main.go",
                "package main\n\
                 func opaque(c *gin.Context) {\n\
                 \tc.JSON(statusFor(x), buildResponse())\n\
                 }\n\
                 func fetched(c *gin.Context) {\n\
                 \tresp := fetch()\n\
                 \tc.JSON(200, resp)\n\
                 }\n\
                 func main() {\n\
                 \tr.GET(\"/opaque\", opaque)\n\
                 \tr.GET(\"/fetched\", fetched)\n\
                 }\n",
            )],
        );
        assert!(
            !source.responses.contains_key("opaque"),
            "a computed status is not a stated one: {:?}",
            source.responses
        );
        let fetched = source.responses.get("fetched").expect("status is stated");
        assert_eq!(
            fetched.statuses[&200],
            WireShape::Unknown,
            "an untyped local states no body shape"
        );
    }

    #[test]
    fn a_fiber_chained_status_claims_its_json_body() {
        use super::super::response_facts::WireShape;
        let source = read_source(
            "fiber_responses",
            &[(
                "main.go",
                "package main\n\
                 type Todo struct {\n\tName string `json:\"name\"`\n}\n\
                 func addTodo(c *fiber.Ctx) error {\n\
                 \treturn c.Status(201).JSON(Todo{Name: \"x\"})\n\
                 }\n\
                 func listTodos(c *fiber.Ctx) error {\n\
                 \treturn c.JSON([]Todo{})\n\
                 }\n\
                 func main() {\n\
                 \tapp.Post(\"/todos\", addTodo)\n\
                 \tapp.Get(\"/todos\", listTodos)\n\
                 }\n",
            )],
        );
        let add = source.responses.get("addTodo").expect("stated");
        assert_eq!(add.statuses[&201], WireShape::Named("Todo".into()));
        let list = source.responses.get("listTodos").expect("stated");
        assert_eq!(
            list.statuses[&200],
            WireShape::Array(Box::new(WireShape::Named("Todo".into()))),
            "an unchained fiber JSON is a 200"
        );
    }

    #[test]
    fn an_embedded_field_makes_the_serializer_abstain() {
        let source = read_source(
            "embedded",
            &[(
                "main.go",
                "package main\n\
                 type Base struct {\n\tID string `json:\"id\"`\n}\n\
                 type Item struct {\n\tBase\n\tName string `json:\"name\"`\n}\n",
            )],
        );
        assert!(
            !source.serializers.contains_key("Item"),
            "promoted fields cannot be enumerated here: {:?}",
            source.serializers.keys()
        );
        assert!(source.serializers.contains_key("Base"));
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
    fn a_group_prefix_does_not_leak_into_another_file() {
        // `r := app.Group("/auth")` in one file put `/auth` on another file's
        // routes: four invented paths, four real ones gone.
        let source = read_source(
            "no_leak",
            &[
                (
                    "auth.go",
                    "package r\nfunc A(app *fiber.App) {\n\tr := app.Group(\"/auth\")\n\
                     \tr.Post(\"/signup\", Signup)\n}\n",
                ),
                (
                    "todo.go",
                    "package r\nfunc T(app *fiber.App) {\n\tr := app.Group(\"/todo\")\n\
                     \tr.Get(\"/list\", List)\n}\n",
                ),
            ],
        );
        let paths: Vec<&String> = source.routes.iter().map(|(path, _, _)| path).collect();
        assert!(paths.contains(&&"/auth/signup".to_string()), "{paths:?}");
        assert!(paths.contains(&&"/todo/list".to_string()), "{paths:?}");
        assert!(
            !paths.contains(&&"/auth/list".to_string()),
            "a neighbour's prefix must not be applied: {paths:?}"
        );
    }

    #[test]
    fn chi_route_and_mount_compose_lexically() {
        let source = read_source(
            "chi_nesting",
            &[(
                "main.go",
                "package main\nfunc main() {\n\tr := chi.NewRouter()\n\
                 \tr.Route(\"/articles\", func(r chi.Router) {\n\
                 \t\tr.Get(\"/search\", Search)\n\t})\n\
                 \tr.Mount(\"/admin\", adminRouter())\n}\n\
                 func adminRouter() http.Handler {\n\tr := chi.NewRouter()\n\
                 \tr.Get(\"/accounts\", Accounts)\n\treturn r\n}\n",
            )],
        );
        let paths: Vec<&String> = source.routes.iter().map(|(path, _, _)| path).collect();
        assert!(
            paths.contains(&&"/articles/search".to_string()),
            "{paths:?}"
        );
        assert!(
            paths.contains(&&"/admin/accounts".to_string()),
            "a mounted router must carry its prefix: {paths:?}"
        );
        assert!(
            !paths.contains(&&"/accounts".to_string()),
            "and must not also surface unprefixed: {paths:?}"
        );
    }

    #[test]
    fn chi_nested_mounts_and_method_router_mounts_keep_every_prefix() {
        let source = read_source(
            "chi_nested_and_method_mounts",
            &[(
                "main.go",
                "package main\n\
                 func main() {\n\
                 \tr := chi.NewRouter()\n\
                 \tr.Route(\"/v3\", func(r chi.Router) {\n\
                 \t\tr.Mount(\"/articles\", articleRouter())\n\
                 \t})\n\
                 \tr.Mount(\"/users\", usersResource{}.Routes())\n\
                 }\n\
                 func articleRouter() http.Handler {\n\
                 \tr := chi.NewRouter()\n\
                 \tr.Get(\"/\", listArticles)\n\
                 \tr.Get(\"/{articleID}\", getArticle)\n\
                 \treturn r\n\
                 }\n\
                 type usersResource struct{}\n\
                 func (usersResource) Routes() chi.Router {\n\
                 \tr := chi.NewRouter()\n\
                 \tr.Get(\"/\", listUsers)\n\
                 \tr.Get(\"/{id}\", getUser)\n\
                 \tr.Get(\"/{id}/sync\", syncUser)\n\
                 \treturn r\n\
                 }\n",
            )],
        );
        let paths: BTreeSet<String> = source
            .routes
            .iter()
            .map(|(path, _, _)| path.clone())
            .collect();
        let expected = BTreeSet::from([
            "/v3/articles/".to_string(),
            "/v3/articles/{articleID}".to_string(),
            "/users/".to_string(),
            "/users/{id}".to_string(),
            "/users/{id}/sync".to_string(),
        ]);
        assert_eq!(paths, expected, "every mounted route needs its full prefix");
    }

    #[test]
    fn a_group_prefix_from_a_constant_skips_its_routes() {
        // The prefix is unknowable, so the routes are real but their location
        // is not. Emitting them at the root names paths nothing serves.
        let source = read_source(
            "opaque_group",
            &[(
                "main.go",
                "package main\nconst adminBase = \"/admin\"\nfunc main() {\n\
                 \tg := r.Group(adminBase)\n\tg.GET(\"/stats\", Stats)\n}\n",
            )],
        );
        assert!(
            source.routes.is_empty(),
            "an unreadable prefix must abstain: {:?}",
            source.routes
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
