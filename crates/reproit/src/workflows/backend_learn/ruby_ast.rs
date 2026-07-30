//! Ruby route and validation extraction over the grammar.
//!
//! Rails states routes and validations as ordinary method calls, which is why
//! a line pattern got so far: `post '/x', to: 'blocks#create'` and
//! `validates :rating, numericality: { ... }` both fit on one line most of the
//! time. What does not fit on one line is the NESTING. A `namespace :api do`
//! block prefixes everything inside it, `scope` does the same, and a validation
//! belongs to the class whose body encloses it. Those are tree facts, and the
//! pattern reader approximated them by tracking the last `class` line it saw.

use super::extract::Family;
use super::field_facts::{drop_ambiguous, record, FieldFact};
use super::grammar::{self, SourceRead, MAX_FIELDS};
use super::route_path::join_mount as join_path;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use tree_sitter::Node;

/// One route before its engine's mount prefix is applied.
type RbRoute = (String, &'static str, Option<String>);

const METHODS: [&str; 7] = ["get", "post", "put", "patch", "delete", "head", "options"];
/// The five routes `resources :name` declares that a draft can exercise.
/// The routes `resources :name` declares, each with the action it serves so
/// `only:`/`except:` can restrict them. `update` answers both PATCH and PUT.
const RESOURCE_ROUTES: [(&str, &str, &str); 8] = [
    ("", "get", "index"),
    ("", "post", "create"),
    ("/new", "get", "new"),
    ("/{id}", "get", "show"),
    ("/{id}", "patch", "update"),
    ("/{id}", "put", "update"),
    ("/{id}", "delete", "destroy"),
    ("/{id}/edit", "get", "edit"),
];

pub(super) fn read(root: &Path) -> SourceRead {
    let mut source = SourceRead::default();
    // class name -> its validated fields.
    let mut shapes: BTreeMap<String, BTreeMap<String, FieldFact>> = BTreeMap::new();
    let mut ambiguous: BTreeSet<String> = BTreeSet::new();
    // action name -> the class whose validations govern it.
    let mut actions: BTreeMap<String, String> = BTreeMap::new();
    let mut found: Vec<(String, &'static str, Option<String>)> = Vec::new();
    // engine key -> the prefix the host app mounts it at.
    let mut mounts: BTreeMap<String, String> = BTreeMap::new();
    // (engine key of the file, its routes), resolved once every file is read:
    // the host's `mount` and the engine's own routes.rb are different files.
    let mut engines: Vec<(String, Vec<RbRoute>)> = Vec::new();

    grammar::read_files(
        root,
        Family::Ruby,
        tree_sitter_ruby::LANGUAGE.into(),
        &mut source,
        |root_node, text, path| {
            routes(root_node, text, "", &mut mounts, &mut found);
            engines.push((engine_of(path), found.split_off(0)));
            classes(root_node, text, &mut shapes, &mut ambiguous, &mut actions);
        },
    );
    drop_ambiguous(&mut shapes, &ambiguous);

    let found: Vec<(String, &'static str, Option<String>)> = engines
        .into_iter()
        .flat_map(|(engine, routes)| {
            let at = mounts.get(&engine).cloned().unwrap_or_default();
            routes.into_iter().map(move |(path, method, handler)| {
                (
                    format!("{}{path}", at.trim_end_matches('/')),
                    method,
                    handler,
                )
            })
        })
        .collect();
    for (path, method, handler) in found {
        source.routes.push((path, method, handler.clone()));
        if let Some(handler) = handler {
            if let Some(fields) = actions
                .get(&handler)
                .and_then(|class| shapes.get(class))
                .cloned()
            {
                source.bodies.insert(handler, fields);
            }
        }
    }
    source
}

/// Which engine a routes file belongs to: `plugins/chat/config/routes.rb` is
/// the `chat` engine, and the host mounts it by that name.
fn engine_of(file: &std::path::Path) -> String {
    let parts: Vec<String> = file
        .components()
        .map(|part| part.as_os_str().to_string_lossy().to_ascii_lowercase())
        .collect();
    parts
        .iter()
        .position(|part| part == "plugins" || part == "engines")
        .and_then(|at| parts.get(at + 1))
        .cloned()
        .unwrap_or_default()
}

/// Route calls under `prefix`, descending into the blocks that extend it.
///
/// Recursion is over the tree rather than over lines, so a `namespace` inside a
/// `scope` composes instead of the inner one winning.
fn routes(
    node: Node,
    text: &str,
    prefix: &str,
    mounts: &mut BTreeMap<String, String>,
    out: &mut Vec<(String, &'static str, Option<String>)>,
) {
    let mut children = Vec::new();
    grammar::children(node, &mut children);
    for child in children {
        if child.kind() != "call" {
            routes(child, text, prefix, mounts, out);
            continue;
        }
        let called = grammar::field(child, text, "method").unwrap_or_default();
        let mut args = Vec::new();
        if let Some(list) = child.child_by_field_name("arguments") {
            grammar::children(list, &mut args);
        }
        let block = child.child_by_field_name("block");
        match called.as_str() {
            // `namespace :api do` and `scope '/api' do` both prefix their body.
            // A namespace takes a SYMBOL, which names a path segment; a scope
            // takes the path itself.
            "namespace" | "scope" => {
                // A scope path may be MULTI-SEGMENT (`scope "/admin/plugins/foo"`)
                // and may arrive as the `path:` keyword rather than positionally.
                // Reading neither dropped the prefix and emitted the body at the
                // root: nine invented paths from one plugin alone.
                let positional = args.first().and_then(|node| segment_of(*node, text));
                let keyword = args.iter().find_map(|node| {
                    (node.kind() == "pair"
                        && grammar::field(*node, text, "key").as_deref() == Some("path"))
                    .then(|| node.child_by_field_name("value"))
                    .flatten()
                    .and_then(|value| segment_of(value, text))
                });
                let inner = match positional.or(keyword) {
                    Some(segment) => format!(
                        "{}/{}",
                        prefix.trim_end_matches('/'),
                        segment.trim_matches('/')
                    ),
                    // A scope with no readable path (`scope module: :api`) does
                    // not change the path, so the body keeps the outer prefix.
                    None => prefix.to_string(),
                };
                if let Some(block) = block {
                    routes(block, text, &inner, mounts, out);
                }
            }
            // `mount ::Chat::Engine, at: "/chat"` puts a whole engine under a
            // prefix. Its routes live in another file, so this only records the
            // mount; unresolved, 22 engines' routes were emitted at the root.
            "mount" => {
                if let Some(at) = args.iter().find_map(|node| {
                    (node.kind() == "pair"
                        && grammar::field(*node, text, "key").as_deref() == Some("at"))
                    .then(|| node.child_by_field_name("value"))
                    .flatten()
                    .and_then(|value| segment_of(value, text))
                }) {
                    if let Some(engine) = args.first().map(|node| grammar::text(*node, text)) {
                        let parts: Vec<&str> = engine
                            .split("::")
                            .filter(|part| !part.is_empty() && *part != "Engine")
                            .collect();
                        let key = parts
                            .last()
                            .copied()
                            .unwrap_or_default()
                            .to_ascii_lowercase();
                        if !key.is_empty() && at != "/" {
                            mounts.insert(key, format!("/{}", at.trim_matches('/')));
                        }
                    }
                }
            }
            "resources" | "resource" => {
                if let Some(name) = resource_segment(&args, text) {
                    let base = format!("{}/{name}", prefix.trim_end_matches('/'));
                    // `only:` and `except:` restrict the set. Expanding all
                    // seven regardless invented routes that 404: 45 such
                    // declarations in one real routes file.
                    let allowed = restricted_actions(&args, text);
                    for (suffix, method, action) in RESOURCE_ROUTES {
                        if allowed.as_ref().is_some_and(|set| !set.contains(action)) {
                            continue;
                        }
                        out.push((format!("{base}{suffix}"), method, None));
                    }
                }
                if let Some(block) = block {
                    // A `collection`/`member` block inside `resources :badges`
                    // hangs off the RESOURCE, not off the enclosing prefix.
                    let base = resource_segment(&args, text)
                        .map(|name| format!("{}/{name}", prefix.trim_end_matches('/')))
                        .unwrap_or_else(|| prefix.to_string());
                    routes(block, text, &base, mounts, out);
                }
            }
            "match" => {
                if let Some((path, action)) = verb_route(&args, text) {
                    for method in match_methods(&args, text) {
                        out.push((join_path(prefix, &path), method, action.clone()));
                    }
                }
                if let Some(block) = block {
                    routes(block, text, prefix, mounts, out);
                }
            }
            "collection" => {
                if let Some(block) = block {
                    routes(block, text, prefix, mounts, out);
                }
            }
            "member" => {
                if let Some(block) = block {
                    routes(block, text, &format!("{prefix}/{{id}}"), mounts, out);
                }
            }
            _ => {
                if let Some(method) = METHODS.into_iter().find(|known| *known == called) {
                    if let Some((path, action)) = verb_route(&args, text) {
                        out.push((join_path(prefix, &path), method, action));
                    }
                }
                if let Some(block) = block {
                    routes(block, text, prefix, mounts, out);
                }
            }
        }
    }
}

/// The path and action a verb call states, in either Rails spelling.
///
/// `get "/x", to: "c#a"` and `get "/x" => "c#a"` are equally common; Discourse
/// writes the hashrocket form 743 times and the comma form 32, and reading only
/// the comma form lost almost the entire routes file. In the hashrocket form
/// the path is the KEY of a pair rather than a positional argument, so it never
/// appeared as the first argument at all.
fn verb_route(args: &[Node], text: &str) -> Option<(String, Option<String>)> {
    // Comma form: the path is the first positional argument.
    if let Some(first) = args.first() {
        if first.kind() == "string" {
            let path = grammar::unquote(grammar::text(*first, text)).to_string();
            if path.starts_with('/') {
                return Some((path, action_of(args, text)));
            }
        }
    }
    // Hashrocket form: `get "/x" => "controller#action"`.
    for argument in args {
        if argument.kind() != "pair" {
            continue;
        }
        let Some(key) = argument.child_by_field_name("key") else {
            continue;
        };
        if key.kind() != "string" {
            continue;
        }
        let path = grammar::unquote(grammar::text(key, text)).to_string();
        if !path.starts_with('/') {
            continue;
        }
        let action = argument
            .child_by_field_name("value")
            .map(|value| grammar::unquote(grammar::text(value, text)).to_string())
            .and_then(|target| target.split_once('#').map(|(_, a)| a.to_string()));
        return Some((path, action));
    }
    None
}

/// `to: 'blocks#create'` -> `create`, the action serving the route.
fn action_of(args: &[Node], text: &str) -> Option<String> {
    for argument in args {
        if argument.kind() != "pair" {
            continue;
        }
        if grammar::field(*argument, text, "key").as_deref() != Some("to") {
            continue;
        }
        let value =
            grammar::unquote(grammar::field(*argument, text, "value")?.as_str()).to_string();
        return value.split_once('#').map(|(_, action)| action.to_string());
    }
    None
}

/// The actions a `resources` call permits, or None when it permits all seven.
fn restricted_actions(args: &[Node], text: &str) -> Option<BTreeSet<String>> {
    const ALL: [&str; 7] = [
        "index", "create", "new", "show", "update", "destroy", "edit",
    ];
    for argument in args {
        if argument.kind() != "pair" {
            continue;
        }
        let key = grammar::field(*argument, text, "key").unwrap_or_default();
        if key != "only" && key != "except" {
            continue;
        }
        let value = argument.child_by_field_name("value")?;
        let named: BTreeSet<String> = symbol_list(value, text);
        if named.is_empty() {
            continue;
        }
        return Some(if key == "only" {
            named
        } else {
            ALL.iter()
                .map(|action| action.to_string())
                .filter(|action| !named.contains(action))
                .collect()
        });
    }
    None
}

/// The literal path segment a resource serves.
///
/// A present but opaque `path:` cannot safely fall back to the resource name:
/// that would emit a path Rails does not serve.
fn resource_segment(args: &[Node], text: &str) -> Option<String> {
    let name = args.first().and_then(|node| segment_of(*node, text))?;
    for argument in args {
        if argument.kind() != "pair"
            || grammar::field(*argument, text, "key").as_deref() != Some("path")
        {
            continue;
        }
        return argument
            .child_by_field_name("value")
            .and_then(|value| segment_of(value, text));
    }
    Some(name)
}

/// Literal HTTP verbs declared by `match "/path", via: ...`.
fn match_methods(args: &[Node], text: &str) -> Vec<&'static str> {
    let Some(via) = args.iter().find_map(|argument| {
        (argument.kind() == "pair"
            && grammar::field(*argument, text, "key").as_deref() == Some("via"))
        .then(|| argument.child_by_field_name("value"))
        .flatten()
    }) else {
        return Vec::new();
    };
    let named = symbol_list(via, text);
    METHODS
        .into_iter()
        .filter(|method| named.contains(*method))
        .collect()
}

/// The symbols in `[:index, :show]` or `%i[index show]`.
fn symbol_list(node: Node, text: &str) -> BTreeSet<String> {
    // The `i` of a `%i[...]` literal is alphanumeric, so trimming punctuation
    // from the ends left it welded to the first symbol and dropped that action.
    let raw = grammar::text(node, text).trim();
    let inner = raw
        .strip_prefix("%i")
        .or_else(|| raw.strip_prefix("%w"))
        .unwrap_or(raw);
    inner
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .filter(|part| !part.is_empty())
        .map(str::to_string)
        .collect()
}

/// A path segment from either a symbol (`:api`) or a string (`'/api'`).
fn segment_of(node: Node, text: &str) -> Option<String> {
    let raw = grammar::text(node, text);
    let value = match node.kind() {
        "simple_symbol" => raw.trim_start_matches(':'),
        "string" => grammar::unquote(raw).trim_start_matches('/'),
        _ => return None,
    };
    // `/admin/plugins/foo` is one scope argument and several path segments.
    let valid = !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '/');
    valid.then(|| value.to_string())
}

/// `validates` calls attributed to the class body that encloses them, and the
/// actions defined alongside them.
fn classes(
    node: Node,
    text: &str,
    shapes: &mut BTreeMap<String, BTreeMap<String, FieldFact>>,
    ambiguous: &mut BTreeSet<String>,
    actions: &mut BTreeMap<String, String>,
) {
    let mut stack = vec![node];
    while let Some(node) = stack.pop() {
        if node.kind() == "class" {
            if let (Some(name), Some(body)) = (
                grammar::field(node, text, "name"),
                node.child_by_field_name("body"),
            ) {
                let mut fields = BTreeMap::new();
                grammar::walk(body, &mut |inner| match inner.kind() {
                    "call" => validates(inner, text, &mut fields),
                    "method" => {
                        if let Some(action) = grammar::field(inner, text, "name") {
                            actions.insert(action, name.clone());
                        }
                    }
                    _ => {}
                });
                if !fields.is_empty() {
                    record(shapes, ambiguous, name, fields);
                }
            }
            // The body was walked whole; nested classes inside it are reached
            // by descending here rather than through that walk, so their
            // validations are attributed to themselves.
        }
        let mut children = Vec::new();
        grammar::children(node, &mut children);
        stack.extend(children);
    }
}

/// `validates :rating, presence: true, numericality: { ... }`
fn validates(node: Node, text: &str, fields: &mut BTreeMap<String, FieldFact>) {
    if grammar::field(node, text, "method").as_deref() != Some("validates") {
        return;
    }
    let Some(list) = node.child_by_field_name("arguments") else {
        return;
    };
    let mut args = Vec::new();
    grammar::children(list, &mut args);
    let Some(name) = args
        .first()
        .filter(|node| node.kind() == "simple_symbol")
        .map(|node| {
            grammar::text(*node, text)
                .trim_start_matches(':')
                .to_string()
        })
    else {
        return;
    };
    if fields.len() >= MAX_FIELDS {
        return;
    }
    let mut fact = FieldFact::default();
    for pair in args.iter().skip(1) {
        if pair.kind() != "pair" {
            continue;
        }
        let key = grammar::field(*pair, text, "key").unwrap_or_default();
        let Some(value) = pair.child_by_field_name("value") else {
            continue;
        };
        match key.as_str() {
            "presence" => fact.required = grammar::text(value, text).trim() == "true",
            "inclusion" => {
                if let Some(values) = inclusion(value, text) {
                    fact.evidence = Some("a validates inclusion rule".to_string());
                    fact.allowed = Some(values);
                }
            }
            "numericality" => {
                if let Some(range) = numericality(value, text) {
                    fact.evidence = Some("a validates numericality rule".to_string());
                    fact.range = Some(range);
                }
            }
            _ => {}
        }
    }
    // A repeated `validates` for one field states MORE about it, so the facts
    // merge: `validates :x, presence: true` then `validates :x, inclusion: ...`
    // is one field with both, not two readings where the last wins.
    let entry = fields.entry(name).or_default();
    entry.required |= fact.required;
    entry.allowed = fact.allowed.or_else(|| entry.allowed.take());
    entry.range = fact.range.or(entry.range);
    entry.evidence = fact.evidence.or_else(|| entry.evidence.take());
}

/// `inclusion: { in: %w[user sponsor] }` or `{ in: ['user', 'sponsor'] }`.
fn inclusion(node: Node, text: &str) -> Option<Vec<String>> {
    let list = grammar::find(node, "string_array").or_else(|| grammar::find(node, "array"))?;
    let mut items = Vec::new();
    grammar::children(list, &mut items);
    let values: Vec<String> = items
        .iter()
        .filter(|item| matches!(item.kind(), "bare_string" | "string" | "simple_symbol"))
        .map(|item| {
            grammar::unquote(grammar::text(*item, text))
                .trim_start_matches(':')
                .to_string()
        })
        .collect();
    (values.len() > 1).then_some(values)
}

/// `numericality: { greater_than_or_equal_to: -1, less_than_or_equal_to: 1 }`
///
/// The exclusive forms are converted to the inclusive integer bound they imply,
/// because the schema vocabulary this compares against has no exclusive form
/// and reporting an off-by-one bound is worse than reporting none.
fn numericality(node: Node, text: &str) -> Option<(Option<f64>, Option<f64>)> {
    let hash = grammar::find(node, "hash")?;
    let mut pairs = Vec::new();
    grammar::children(hash, &mut pairs);
    let mut low = None;
    let mut high = None;
    for pair in pairs {
        if pair.kind() != "pair" {
            continue;
        }
        let key = grammar::field(pair, text, "key").unwrap_or_default();
        let Some(value) = grammar::field(pair, text, "value").and_then(|raw| grammar::number(&raw))
        else {
            continue;
        };
        match key.as_str() {
            "greater_than_or_equal_to" => low = Some(value),
            "less_than_or_equal_to" => high = Some(value),
            "greater_than" => low = Some(value + 1.0),
            "less_than" => high = Some(value - 1.0),
            _ => {}
        }
    }
    (low.is_some() || high.is_some()).then_some((low, high))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_source(case: &str, files: &[(&str, &str)]) -> SourceRead {
        let root =
            std::env::temp_dir().join(format!("reproit-rbast-{}-{case}", std::process::id()));
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
    fn a_route_resolves_the_validations_of_the_class_its_action_lives_in() {
        let source = read_source(
            "validates",
            &[
                ("routes.rb", "post '/v1/blocks', to: 'blocks#create'\n"),
                (
                    "block.rb",
                    "class Block < ApplicationRecord\n\
                     \x20 validates :blocked_type, presence: true, inclusion: { in: %w[user sponsor] }\n\
                     \x20 validates :rating, numericality: { greater_than_or_equal_to: -1, less_than_or_equal_to: 1 }\n\
                     \x20 def create\n  end\nend\n",
                ),
            ],
        );
        assert_eq!(
            source.routes,
            vec![("/v1/blocks".to_string(), "post", Some("create".into()))]
        );
        let fields = source.bodies.get("create").expect("resolved");
        assert_eq!(
            fields["blocked_type"].allowed.as_deref(),
            Some(["user".to_string(), "sponsor".to_string()].as_slice())
        );
        assert!(fields["blocked_type"].required);
        assert_eq!(fields["rating"].range, Some((Some(-1.0), Some(1.0))));
    }

    #[test]
    fn a_namespace_block_prefixes_every_route_inside_it() {
        let source = read_source(
            "namespace",
            &[(
                "routes.rb",
                "Rails.application.routes.draw do\n  namespace :api do\n\
                 \x20   scope '/v1' do\n      get '/ping', to: 'ping#show'\n    end\n  end\nend\n",
            )],
        );
        assert_eq!(
            source.routes,
            vec![("/api/v1/ping".to_string(), "get", Some("show".into()))]
        );
    }

    #[test]
    fn resources_expands_under_its_enclosing_prefix() {
        let source = read_source(
            "resources",
            &[("routes.rb", "namespace :api do\n  resources :users\nend\n")],
        );
        let paths: Vec<&String> = source.routes.iter().map(|(path, _, _)| path).collect();
        assert!(
            paths.iter().all(|path| path.starts_with("/api/users")),
            "{paths:?}"
        );
        assert_eq!(source.routes.len(), 8, "{paths:?}");
    }

    #[test]
    fn two_validates_for_one_field_state_more_rather_than_replacing() {
        let source = read_source(
            "merge",
            &[
                ("routes.rb", "post '/x', to: 'b#create'\n"),
                (
                    "b.rb",
                    "class B\n  validates :x, presence: true\n\
                     \x20 validates :x, inclusion: { in: %w[a b] }\n  def create\n  end\nend\n",
                ),
            ],
        );
        let fields = source.bodies.get("create").expect("resolved");
        assert!(
            fields["x"].required,
            "presence must survive the second call"
        );
        assert!(fields["x"].allowed.is_some(), "inclusion must be read too");
    }

    #[test]
    fn an_exclusive_bound_becomes_the_inclusive_one_it_implies() {
        let source = read_source(
            "exclusive",
            &[
                ("routes.rb", "post '/x', to: 'b#create'\n"),
                (
                    "b.rb",
                    "class B\n  validates :n, numericality: { greater_than: 0, less_than: 6 }\n\
                     \x20 def create\n  end\nend\n",
                ),
            ],
        );
        let fields = source.bodies.get("create").expect("resolved");
        assert_eq!(fields["n"].range, Some((Some(1.0), Some(5.0))));
    }

    #[test]
    fn two_classes_of_the_same_name_resolve_to_neither() {
        let source = read_source(
            "ambiguous",
            &[
                ("routes.rb", "post '/x', to: 'b#create'\n"),
                (
                    "a.rb",
                    "class B\n  validates :a, presence: true\n  def create\n  end\nend\n",
                ),
                ("c.rb", "class B\n  validates :c, presence: true\nend\n"),
            ],
        );
        assert!(
            !source.bodies.contains_key("create"),
            "an ambiguous class must abstain: {:?}",
            source.bodies
        );
    }

    #[test]
    fn the_hashrocket_form_is_a_route_and_only_restricts_a_resource() {
        // Discourse writes `get "/x" => "c#a"` 743 times; reading only the
        // comma form lost almost the entire routes file. `only:` restricts the
        // expansion, which otherwise invents routes that 404.
        let source = read_source(
            "hashrocket",
            &[(
                "routes.rb",
                "Rails.application.routes.draw do\n\
                 \x20 get \"/404-body\" => \"exceptions#not_found_body\"\n\
                 \x20 post \"/webhooks/aws\" => \"webhooks#aws\"\n\
                 \x20 resources :about, only: [:index]\n\
                 \x20 resources :words, only: %i[index create destroy]\nend\n",
            )],
        );
        let paths: Vec<&String> = source.routes.iter().map(|(path, _, _)| path).collect();
        assert!(paths.contains(&&"/404-body".to_string()), "{paths:?}");
        assert!(paths.contains(&&"/webhooks/aws".to_string()), "{paths:?}");
        assert_eq!(
            source
                .routes
                .iter()
                .filter(|(p, _, _)| p == "/about")
                .count(),
            1,
            "only: [:index] is one operation: {paths:?}"
        );
        assert!(
            !paths.contains(&&"/about/{id}".to_string()),
            "show/update/destroy are not in only: {paths:?}"
        );
        // %i[...] must not weld its `i` onto the first symbol.
        assert_eq!(
            source
                .routes
                .iter()
                .filter(|(p, _, _)| p == "/words")
                .count(),
            2,
            "index and create: {paths:?}"
        );
    }

    #[test]
    fn rails_match_and_resource_options_preserve_the_served_surface() {
        let source = read_source(
            "match-and-resource-options",
            &[(
                "routes.rb",
                "Rails.application.routes.draw do\n\
                 \x20 match \"/404\", via: %i[get post], to: \"errors#not_found\"\n\
                 \x20 resources :ai_llm_quotas, path: \"quotas\", only: %i[index show]\n\
                 \x20 resources :drafts, except: %i[destroy]\n\
                 end\n",
            )],
        );
        let operations: BTreeSet<(String, &'static str)> = source
            .routes
            .iter()
            .map(|(path, method, _)| (path.clone(), *method))
            .collect();
        assert!(
            operations.contains(&("/404".to_string(), "get"))
                && operations.contains(&("/404".to_string(), "post")),
            "match via: must emit every literal method: {operations:?}"
        );
        assert!(
            operations.contains(&("/quotas".to_string(), "get"))
                && operations.contains(&("/quotas/{id}".to_string(), "get")),
            "resources path: must replace the resource segment: {operations:?}"
        );
        assert!(
            !operations
                .iter()
                .any(|(path, _)| path.starts_with("/ai_llm_quotas")),
            "the resource name is not a served path when path: overrides it: {operations:?}"
        );
        assert!(
            operations.contains(&("/drafts/{id}/edit".to_string(), "get")),
            "except: must preserve the edit action: {operations:?}"
        );
    }

    #[test]
    fn a_file_that_does_not_parse_is_counted() {
        let source = read_source(
            "broken",
            &[("ok.rb", "x = 1\n"), ("bad.rb", "def broken(\n")],
        );
        assert_eq!(source.files_parsed, 1);
        assert_eq!(source.files_unreadable, 1);
    }

    #[test]
    fn a_non_route_call_with_a_string_is_not_a_route() {
        let source = read_source("notaroute", &[("app.rb", "puts '/not/a/route'\n")]);
        assert!(source.routes.is_empty(), "{:?}", source.routes);
    }
}
