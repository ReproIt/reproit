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
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use tree_sitter::Node;

const METHODS: [&str; 5] = ["get", "post", "put", "patch", "delete"];
/// The five routes `resources :name` declares that a draft can exercise.
const RESOURCE_ROUTES: [(&str, &str); 5] = [
    ("", "get"),
    ("", "post"),
    ("/{id}", "get"),
    ("/{id}", "patch"),
    ("/{id}", "delete"),
];

pub(super) fn read(root: &Path) -> SourceRead {
    let mut source = SourceRead::default();
    // class name -> its validated fields.
    let mut shapes: BTreeMap<String, BTreeMap<String, FieldFact>> = BTreeMap::new();
    let mut ambiguous: BTreeSet<String> = BTreeSet::new();
    // action name -> the class whose validations govern it.
    let mut actions: BTreeMap<String, String> = BTreeMap::new();
    let mut found: Vec<(String, &'static str, Option<String>)> = Vec::new();

    grammar::read_files(
        root,
        Family::Ruby,
        tree_sitter_ruby::LANGUAGE.into(),
        &mut source,
        |root_node, text| {
            routes(root_node, text, "", &mut found);
            classes(root_node, text, &mut shapes, &mut ambiguous, &mut actions);
        },
    );
    drop_ambiguous(&mut shapes, &ambiguous);

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

/// Route calls under `prefix`, descending into the blocks that extend it.
///
/// Recursion is over the tree rather than over lines, so a `namespace` inside a
/// `scope` composes instead of the inner one winning.
fn routes(
    node: Node,
    text: &str,
    prefix: &str,
    out: &mut Vec<(String, &'static str, Option<String>)>,
) {
    let mut children = Vec::new();
    grammar::children(node, &mut children);
    for child in children {
        if child.kind() != "call" {
            routes(child, text, prefix, out);
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
                let segment = args.first().map(|node| segment_of(*node, text));
                let inner = match segment {
                    Some(Some(segment)) => format!("{}/{segment}", prefix.trim_end_matches('/')),
                    // A scope with no readable path (`scope module: :api`) does
                    // not change the path, so the body keeps the outer prefix.
                    _ => prefix.to_string(),
                };
                if let Some(block) = block {
                    routes(block, text, &inner, out);
                }
            }
            "resources" | "resource" => {
                if let Some(Some(name)) = args.first().map(|node| segment_of(*node, text)) {
                    let base = format!("{}/{name}", prefix.trim_end_matches('/'));
                    for (suffix, method) in RESOURCE_ROUTES {
                        out.push((format!("{base}{suffix}"), method, None));
                    }
                }
                if let Some(block) = block {
                    routes(block, text, prefix, out);
                }
            }
            _ => {
                if let Some(method) = METHODS.into_iter().find(|known| *known == called) {
                    if let Some(path) = args
                        .first()
                        .map(|node| grammar::unquote(grammar::text(*node, text)))
                        .filter(|path| path.starts_with('/'))
                    {
                        let full = format!("{}{path}", prefix.trim_end_matches('/'));
                        out.push((full, method, action_of(&args, text)));
                    }
                }
                if let Some(block) = block {
                    routes(block, text, prefix, out);
                }
            }
        }
    }
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

/// A path segment from either a symbol (`:api`) or a string (`'/api'`).
fn segment_of(node: Node, text: &str) -> Option<String> {
    let raw = grammar::text(node, text);
    let value = match node.kind() {
        "simple_symbol" => raw.trim_start_matches(':'),
        "string" => grammar::unquote(raw).trim_start_matches('/'),
        _ => return None,
    };
    let valid = !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
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
        assert_eq!(source.routes.len(), 5, "{paths:?}");
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
