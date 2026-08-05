//! Query parameters a Python handler states, split from `python_ast` at the
//! same boundary as `node_body`: that file resolves the routes, this one reads
//! what one handler accepts from the query string.
//!
//! Two signals, both explicit in source. FastAPI infers a query parameter from
//! a scalar-annotated signature argument that is not a path parameter, so the
//! signature is read the same way the framework reads it. Flask and Django
//! state the read itself: `request.args.get('q')` and `request.GET['q']` name
//! the parameter at the call site, and the subscript form demands it.

use super::field_facts::{drop_ambiguous, record, FieldFact};
use super::grammar::{self, MAX_FIELDS};
use std::collections::{BTreeMap, BTreeSet};
use tree_sitter::Node;

/// Query parameters as the route walk collects them, keyed by handler name.
///
/// Signature facts come from a decorated definition, whose route
/// disambiguates it; bare body reads are keyed by function name alone, so a
/// conflicting redefinition abstains the same way a model does.
#[derive(Default)]
pub(super) struct Queries {
    signatures: BTreeMap<String, BTreeMap<String, FieldFact>>,
    reads: BTreeMap<String, BTreeMap<String, FieldFact>>,
    ambiguous: BTreeSet<String>,
}

impl Queries {
    /// Read one routed, decorated definition's signature.
    pub(super) fn take_signature(
        &mut self,
        handler: &str,
        definition: Option<Node>,
        text: &str,
        raw_paths: &[String],
    ) {
        let Some(definition) = definition else { return };
        let path_params = path_param_names(raw_paths);
        let fields = signature_queries(definition, text, &path_params);
        if !fields.is_empty() {
            self.signatures.insert(handler.to_string(), fields);
        }
    }

    /// Read one function body's `request.args` / `request.GET` reads.
    pub(super) fn take_reads(&mut self, handler: &str, definition: Node, text: &str) {
        let fields = request_reads(definition, text);
        if !fields.is_empty() {
            record(
                &mut self.reads,
                &mut self.ambiguous,
                handler.to_string(),
                fields,
            );
        }
    }

    /// The per-handler query parameters, signature facts winning per name:
    /// the framework's own reading of the same declaration.
    pub(super) fn resolve(
        mut self,
        routes: &[(String, &'static str, Option<String>)],
    ) -> BTreeMap<String, BTreeMap<String, FieldFact>> {
        drop_ambiguous(&mut self.reads, &self.ambiguous);
        let handlers: BTreeSet<String> = routes
            .iter()
            .filter_map(|(_, _, handler)| handler.clone())
            .collect();
        let mut resolved = BTreeMap::new();
        for handler in handlers {
            let mut fields = self.signatures.remove(&handler).unwrap_or_default();
            if let Some(reads) = self.reads.get(&handler) {
                for (name, fact) in reads {
                    fields.entry(name.clone()).or_insert_with(|| fact.clone());
                }
            }
            if !fields.is_empty() {
                resolved.insert(handler, fields);
            }
        }
        resolved
    }
}

/// The parameter names a set of raw decorator paths bind: `{item_id}` and the
/// Flask converter forms `<item_id>` / `<int:item_id>`.
fn path_param_names(paths: &[String]) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for path in paths {
        for segment in path.split('/') {
            let inner = segment
                .strip_prefix('{')
                .and_then(|rest| rest.strip_suffix('}'))
                .or_else(|| {
                    segment
                        .strip_prefix('<')
                        .and_then(|rest| rest.strip_suffix('>'))
                });
            if let Some(inner) = inner {
                let name = inner.split(':').next_back().unwrap_or(inner);
                names.insert(name.to_string());
            }
        }
    }
    names
}

/// FastAPI query parameters from one decorated function's signature: a
/// scalar-annotated argument that names no path parameter. A model annotation
/// is the body, an untyped argument states nothing, and a `Depends`-style
/// default is an injection, not an input.
fn signature_queries(
    definition: Node,
    text: &str,
    path_params: &BTreeSet<String>,
) -> BTreeMap<String, FieldFact> {
    let mut fields = BTreeMap::new();
    let Some(list) = definition.child_by_field_name("parameters") else {
        return fields;
    };
    let mut cursor = list.walk();
    for parameter in list.children(&mut cursor) {
        if fields.len() >= MAX_FIELDS {
            break;
        }
        let (name, annotation, default) = match parameter.kind() {
            "typed_parameter" => {
                let mut inner = parameter.walk();
                let name = parameter
                    .children(&mut inner)
                    .find(|child| child.kind() == "identifier")
                    .map(|node| grammar::text(node, text).to_string());
                (name, grammar::field(parameter, text, "type"), None)
            }
            "typed_default_parameter" => (
                grammar::field(parameter, text, "name"),
                grammar::field(parameter, text, "type"),
                grammar::field(parameter, text, "value"),
            ),
            _ => continue,
        };
        let (Some(name), Some(annotation)) = (name, annotation) else {
            continue;
        };
        if path_params.contains(&name) || !scalar_annotation(&annotation) {
            continue;
        }
        let default = default.unwrap_or_default();
        // A dependency or an explicitly non-query default is not a query
        // parameter, whatever its annotation says.
        const NOT_QUERY: [&str; 7] = [
            "Depends(", "Body(", "Path(", "Header(", "Cookie(", "Form(", "File(",
        ];
        if NOT_QUERY.iter().any(|marker| default.contains(marker)) {
            continue;
        }
        // No default demands the value; `Query(...)` spells the same demand.
        let required = default.is_empty()
            || default
                .strip_prefix("Query(")
                .is_some_and(|rest| rest.trim_start().starts_with("..."));
        fields.insert(
            name,
            FieldFact {
                required,
                evidence: Some("a scalar-annotated handler parameter".to_string()),
                ..FieldFact::default()
            },
        );
    }
    fields
}

/// Whether an annotation names a query-string scalar: `int`, `str`, `float`,
/// or `bool`, possibly wrapped in `Optional[...]` or `... | None`.
fn scalar_annotation(annotation: &str) -> bool {
    let inner = annotation.trim();
    let inner = inner
        .strip_prefix("Optional[")
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(inner);
    let inner = inner
        .strip_suffix("| None")
        .or_else(|| inner.strip_prefix("None |"))
        .unwrap_or(inner);
    matches!(inner.trim(), "int" | "str" | "float" | "bool")
}

/// Flask and Django query reads in one function body: `request.args.get('q')`
/// and `request.GET.get('q')` are optional, `request.args['q']` and
/// `request.GET['q']` demand the key or raise.
fn request_reads(definition: Node, text: &str) -> BTreeMap<String, FieldFact> {
    let mut fields = BTreeMap::new();
    let Some(body) = definition.child_by_field_name("body") else {
        return fields;
    };
    grammar::walk(body, &mut |node| {
        if fields.len() >= MAX_FIELDS {
            return;
        }
        match node.kind() {
            "call" => {
                let Some(function) = node.child_by_field_name("function") else {
                    return;
                };
                let callee = grammar::text(function, text);
                let source = ["request.args.get", "request.GET.get"]
                    .into_iter()
                    .find(|suffix| callee.ends_with(suffix));
                let Some(source) = source else {
                    return;
                };
                let Some(name) = first_string_argument(node, text) else {
                    return;
                };
                fields.entry(name).or_insert(FieldFact {
                    required: false,
                    evidence: Some(format!("a {source}(...) read")),
                    ..FieldFact::default()
                });
            }
            "subscript" => {
                let Some(value) = node.child_by_field_name("value") else {
                    return;
                };
                let object = grammar::text(value, text);
                let source = ["request.args", "request.GET"]
                    .into_iter()
                    .find(|suffix| object.ends_with(suffix));
                let Some(source) = source else {
                    return;
                };
                let Some(name) = node
                    .child_by_field_name("subscript")
                    .filter(|key| key.kind() == "string")
                    .and_then(|key| super::python_ast::string_value(key, text))
                else {
                    return;
                };
                // The subscript raises on absence, which is a demand; a `.get`
                // of the same key elsewhere does not weaken it.
                fields.insert(
                    name,
                    FieldFact {
                        required: true,
                        evidence: Some(format!("a {source}[...] read")),
                        ..FieldFact::default()
                    },
                );
            }
            _ => {}
        }
    });
    fields
}

fn first_string_argument(call: Node, text: &str) -> Option<String> {
    let arguments = call.child_by_field_name("arguments")?;
    let mut cursor = arguments.walk();
    let first = arguments
        .children(&mut cursor)
        .find(|child| child.is_named())?;
    (first.kind() == "string")
        .then(|| super::python_ast::string_value(first, text))
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::super::grammar::SourceRead;
    use super::super::python_ast;

    fn read_source(case: &str, files: &[(&str, &str)]) -> SourceRead {
        let root =
            std::env::temp_dir().join(format!("reproit-pyquery-{}-{case}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("root");
        for (name, body) in files {
            std::fs::write(root.join(name), body).expect("write");
        }
        let source = python_ast::read(&root);
        let _ = std::fs::remove_dir_all(&root);
        source
    }

    #[test]
    fn a_fastapi_signature_states_its_query_parameters() {
        let source = read_source(
            "fastapi",
            &[(
                "main.py",
                "from fastapi import FastAPI\napp = FastAPI()\n\
                 @app.get(\"/items/{item_id}\")\n\
                 async def read_item(item_id: str, q: str, limit: int = 10):\n    return {}\n",
            )],
        );
        let fields = source.queries.get("read_item").expect("stated");
        assert!(
            !fields.contains_key("item_id"),
            "a path parameter is not a query parameter: {:?}",
            fields.keys()
        );
        assert!(fields["q"].required, "no default demands the value");
        assert!(!fields["limit"].required, "a default makes it optional");
    }

    #[test]
    fn a_model_annotation_and_a_dependency_are_not_query_parameters() {
        let source = read_source(
            "not_query",
            &[(
                "main.py",
                "@app.post(\"/items\")\n\
                 async def create(body: ItemRequest, db: str = Depends(get_db)):\n    return {}\n",
            )],
        );
        assert!(
            !source.queries.contains_key("create"),
            "{:?}",
            source.queries
        );
    }

    #[test]
    fn a_flask_request_args_read_names_the_parameter() {
        let source = read_source(
            "flask",
            &[(
                "app.py",
                "@app.route(\"/search\")\ndef search():\n\
                 \x20   q = request.args.get('q')\n\
                 \x20   page = request.args['page']\n    return {}\n",
            )],
        );
        let fields = source.queries.get("search").expect("stated");
        assert!(!fields["q"].required, "a .get read is optional");
        assert!(fields["page"].required, "a subscript read demands the key");
    }

    #[test]
    fn a_django_view_names_its_request_get_reads() {
        let source = read_source(
            "django",
            &[
                (
                    "urls.py",
                    "from django.urls import path\nfrom . import views\n\
                     urlpatterns = [\n    path('search/', views.search),\n]\n",
                ),
                (
                    "views.py",
                    "def search(request):\n    q = request.GET.get('q')\n\
                     \x20   kind = request.GET['kind']\n    return None\n",
                ),
            ],
        );
        let fields = source.queries.get("search").expect("stated");
        assert!(!fields["q"].required);
        assert!(fields["kind"].required);
    }
}
