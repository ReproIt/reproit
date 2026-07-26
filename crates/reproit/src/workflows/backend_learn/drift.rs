//! Declared contract versus the routes the source actually serves.
//!
//! Nothing checked a hand-written schema against the code, so a schema that
//! merely LOOKS right silently cost coverage: a `blocked_type: {type: string}`
//! where the handler accepts an enum rejects every generated value, and the run
//! reports "exercised" while evaluating nothing. Worse, a path the service does
//! not serve 404s forever and reads as a passing operation.
//!
//! `--learn` already extracts routes from source for exactly this reason. This
//! points the same extractor at VALIDATION: which declared operations have no
//! matching route, and which served routes are undeclared. It compares
//! (method, path template) only, never types, because the extractor sees routes
//! and not handler signatures, and reporting a type mismatch it cannot actually
//! observe would be the same overclaiming the schema is guilty of.

use super::declarative_types;
use super::extract::{self, Derived};
use super::field_facts;
use super::go_types;
use super::node_types;
use super::python_types;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// One (method, path) the schema declares or the source serves.
pub type Route = (String, String);

/// A family's reader: handler name -> the body fields it accepts. Boxed so each
/// family plugs in the same way and adding one is a match arm, not plumbing.
type BodyReader<'a> = Box<dyn Fn(&str) -> Option<BTreeMap<String, field_facts::FieldFact>> + 'a>;

#[derive(Debug, Default, PartialEq)]
pub struct Drift {
    /// Declared in a schema, no matching route in the source. These 404 at
    /// runtime, so every attempt is wasted.
    pub undeclared_by_source: Vec<Route>,
    /// Served by the source, absent from every schema. Real surface nothing
    /// will ever test.
    pub unserved_by_schema: Vec<Route>,
    /// A declared request-body field the handler's type disagrees with. The
    /// route is right, so every attempt reaches the service and every one is
    /// rejected, which reads as "exercised" while evaluating nothing.
    pub field_mismatches: Vec<FieldMismatch>,
    /// Operations that matched. Reported so a clean result is a positive
    /// statement rather than the absence of a warning.
    pub matched: usize,
    pub files_scanned: usize,
    /// Source files the reader could not read. While this is non-zero the
    /// "no route matched" direction is not evidence of anything, because the
    /// route may be in a file that was never read.
    pub unreadable_sources: usize,
    /// Whether this family has a type reader at all.
    pub types_checked: bool,
    /// How many declared operations actually had their request body compared.
    /// An operation whose handler could not be resolved was NOT checked, and a
    /// clean result must not speak for it.
    pub bodies_compared: usize,
}

/// One declared body field the code contradicts.
#[derive(Debug, PartialEq)]
pub struct FieldMismatch {
    pub operation: Route,
    pub field: String,
    pub detail: String,
}

impl Drift {
    pub fn is_clean(&self) -> bool {
        self.undeclared_by_source.is_empty()
            && self.unserved_by_schema.is_empty()
            && self.field_mismatches.is_empty()
    }
}

/// The (method, path) pairs an OpenAPI document declares.
///
/// Only OpenAPI: a GraphQL or protobuf service has no URL routes to compare
/// against, and inventing a comparison for them would produce noise rather than
/// drift. Those return an empty list, which the caller reports as "not checked".
pub fn declared_routes(document: &serde_json::Value) -> Vec<Route> {
    const METHODS: [&str; 7] = ["get", "post", "put", "patch", "delete", "head", "options"];
    let Some(paths) = document.get("paths").and_then(|paths| paths.as_object()) else {
        return Vec::new();
    };
    let mut routes = Vec::new();
    for (path, item) in paths {
        let Some(operations) = item.as_object() else {
            continue;
        };
        for method in operations.keys() {
            if METHODS.contains(&method.as_str()) {
                routes.push((method.to_uppercase(), path.clone()));
            }
        }
    }
    routes.sort();
    routes
}

/// Compare declared operations against routes extracted from `root`.
///
/// Returns None when the source cannot be read for this framework, so the
/// caller reports "not checked" rather than inventing a clean result: an
/// extractor that found nothing must never look like a schema that matches.
pub fn compare(
    root: &Path,
    framework: &str,
    declared: &[Route],
    document: &serde_json::Value,
) -> Option<Drift> {
    let derived = extract::derive(root, framework)?;
    if derived.routes.is_empty() {
        return None;
    }
    let mut drift = diff(declared, &derived);
    drift.unreadable_sources = derived.unreadable;
    // Types are read per family. A family with no reader abstains rather than
    // guessing: a check that cannot see handler signatures must not imply it
    // looked at them.
    let fields: Option<BodyReader> = match extract::family_for(framework) {
        Some(extract::Family::Rust) => {
            let bodies = derived.bodies.clone();
            Some(Box::new(move |handler| bodies.get(handler).cloned()))
        }
        Some(extract::Family::Python) => {
            let types = python_types::scan_types(root);
            Some(Box::new(move |handler| types.body_fields(handler)))
        }
        Some(extract::Family::Go) => {
            let types = go_types::scan_types(root);
            Some(Box::new(move |handler| types.body_fields(handler)))
        }
        Some(extract::Family::Node) => {
            let types = node_types::scan_types(root);
            Some(Box::new(move |handler| types.body_fields(handler)))
        }
        Some(extract::Family::Ruby) => {
            let types = declarative_types::scan_types(root, declarative_types::Family::Ruby);
            Some(Box::new(move |handler| types.body_fields(handler)))
        }
        Some(extract::Family::Php) => {
            let types = declarative_types::scan_types(root, declarative_types::Family::Php);
            Some(Box::new(move |handler| types.body_fields(handler)))
        }
        Some(extract::Family::Spring) => {
            let types = declarative_types::scan_types(root, declarative_types::Family::Java);
            Some(Box::new(move |handler| types.body_fields(handler)))
        }
        None => None,
    };
    if let Some(fields) = fields {
        let (mismatches, compared) = compare_fields(document, &derived, fields.as_ref());
        drift.field_mismatches = mismatches;
        drift.bodies_compared = compared;
        drift.types_checked = true;
    }
    Some(drift)
}

/// Bound the scan for sibling services.
const MAX_SERVICE_SCAN: usize = 64;

/// Which subtree implements the service this config describes.
///
/// `backend.source` when declared. Otherwise the project root, EXCEPT when the
/// root turns out to hold several services, where scanning it would compare
/// this service's schema against a sibling's routes. That produced advice that
/// was confidently wrong in the dangerous direction ("delete the operation" for
/// a correct one), so an undeclared multi-service root abstains instead.
pub enum SourceRoot {
    Scan(std::path::PathBuf),
    /// Several services under one root and no `backend.source` to pick one.
    Ambiguous(Vec<String>),
}

pub fn source_root(project_root: &Path, declared: Option<&str>) -> SourceRoot {
    if let Some(declared) = declared {
        return SourceRoot::Scan(project_root.join(declared));
    }
    let siblings = sibling_services(project_root);
    if siblings.len() > 1 {
        return SourceRoot::Ambiguous(siblings);
    }
    SourceRoot::Scan(project_root.to_path_buf())
}

/// Immediate child directories that independently detect as their own backend.
/// A Cargo workspace whose members are the only services still reads as one
/// service, because the members are not children of the root.
fn sibling_services(project_root: &Path) -> Vec<String> {
    use crate::adapters::project_scaffold::backend_detect::detect_backend_framework;
    let Ok(entries) = std::fs::read_dir(project_root) else {
        return Vec::new();
    };
    let mut paths: Vec<std::path::PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_dir()
                && !path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with('.'))
        })
        .take(MAX_SERVICE_SCAN)
        .collect();
    paths.sort();
    paths
        .iter()
        .filter(|path| detect_backend_framework(path).is_some())
        .filter_map(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .map(str::to_string)
        })
        .collect()
}

fn diff(declared: &[Route], derived: &Derived) -> Drift {
    let served: BTreeSet<Route> = derived
        .routes
        .iter()
        .flat_map(|(path, methods)| {
            methods
                .iter()
                .map(move |method| (method.to_uppercase(), path.clone()))
        })
        .collect();
    // Path parameters are named by the handler, not by the schema, so
    // `/users/{id}` and `/users/{user_id}` are the same route. Compare on a
    // name-erased shape and keep the declared spelling for the message.
    let served_shapes: BTreeMap<Route, Route> = served
        .iter()
        .map(|route| (erase_params(route), route.clone()))
        .collect();
    let declared_set: BTreeSet<Route> = declared
        .iter()
        .map(|(method, path)| (method.to_uppercase(), path.clone()))
        .collect();
    let declared_shapes: BTreeSet<Route> = declared_set.iter().map(erase_params).collect();

    let mut drift = Drift {
        files_scanned: derived.files_scanned,
        ..Drift::default()
    };
    for route in &declared_set {
        if served_shapes.contains_key(&erase_params(route)) {
            drift.matched += 1;
        } else {
            drift.undeclared_by_source.push(route.clone());
        }
    }
    for (shape, route) in &served_shapes {
        if !declared_shapes.contains(shape) {
            drift.unserved_by_schema.push(route.clone());
        }
    }
    drift
}

/// `/users/{user_id}/posts/{id}` -> `/users/{}/posts/{}`.
fn erase_params(route: &Route) -> Route {
    let mut erased = String::with_capacity(route.1.len());
    let mut in_param = false;
    for character in route.1.chars() {
        match character {
            '{' => {
                in_param = true;
                erased.push_str("{}");
            }
            '}' => in_param = false,
            _ if !in_param => erased.push(character),
            _ => {}
        }
    }
    (route.0.clone(), erased)
}

/// The human report. Silent only when the comparison actually ran and matched.
pub fn lines(drift: &Drift) -> Vec<String> {
    let mut lines = Vec::new();
    // The two directions are NOT equally certain, and must not be reported as
    // if they were.
    //
    // "served but not declared" rests on a route the extractor FOUND: positive
    // evidence, safe to state. "declared but not served" rests on the extractor
    // not matching anything, and a pattern-based reader cannot know what it
    // failed to match, so the same absence is produced by a route it does not
    // understand. Telling someone to delete an operation on that basis is the
    // one mistake this check must never make: the operation is usually real.
    for (label, routes, fix) in [
        (
            "declared, but no route matched in source",
            &drift.undeclared_by_source,
            match drift.unreadable_sources {
                // A file that would not parse is a known blind spot, so this
                // list is not evidence at all while one exists.
                0 => "worth checking: the path may be wrong, or the route may be \
                      built in a way the reader does not follow"
                    .to_string(),
                unreadable => format!(
                    "NOT reliable: {unreadable} source file(s) could not be parsed, so a \
                     route may simply never have been read. Fix those first"
                ),
            }
            .as_str(),
        ),
        (
            "served by the source but not declared",
            &drift.unserved_by_schema,
            "add these to a schema so they are actually tested",
        ),
    ] {
        if routes.is_empty() {
            continue;
        }
        lines.push(format!("{} ({}): {}", label, routes.len(), fix));
        for (method, path) in routes.iter().take(MAX_REPORTED) {
            lines.push(format!("      {method} {path}"));
        }
        if routes.len() > MAX_REPORTED {
            lines.push(format!(
                "      ... and {} more",
                routes.len() - MAX_REPORTED
            ));
        }
    }
    if !drift.field_mismatches.is_empty() {
        lines.push(format!(
            "declared body fields the handler disagrees with ({}): every request is rejected, \
             so the operation reads as exercised while evaluating nothing",
            drift.field_mismatches.len()
        ));
        for mismatch in drift.field_mismatches.iter().take(MAX_REPORTED) {
            lines.push(format!(
                "      {} {} .{}: {}",
                mismatch.operation.0, mismatch.operation.1, mismatch.field, mismatch.detail
            ));
        }
        if drift.field_mismatches.len() > MAX_REPORTED {
            lines.push(format!(
                "      ... and {} more",
                drift.field_mismatches.len() - MAX_REPORTED
            ));
        }
    }
    lines
}

const MAX_REPORTED: usize = 15;

/// Render a bound pair the way a reader thinks about it.
fn describe_bounds(low: Option<f64>, high: Option<f64>) -> String {
    fn number(value: f64) -> String {
        if value.fract() == 0.0 {
            format!("{}", value as i64)
        } else {
            format!("{value}")
        }
    }
    match (low, high) {
        (Some(low), Some(high)) => format!("{}..{}", number(low), number(high)),
        (Some(low), None) => format!(">= {}", number(low)),
        (None, Some(high)) => format!("<= {}", number(high)),
        (None, None) => "no bound".to_string(),
    }
}

/// Compare each declared request body against the handler's Rust types.
///
/// Only the three things source can actually settle: a value set the schema
/// leaves open, a field the struct does not have, and a field the handler
/// requires that the schema does not. Anything the type does not decide (a
/// range check inside the handler, a validator attribute) is left alone, so a
/// reported mismatch is always something the compiler already knows.
fn compare_fields(
    document: &serde_json::Value,
    derived: &Derived,
    body_fields: &dyn Fn(&str) -> Option<BTreeMap<String, field_facts::FieldFact>>,
) -> (Vec<FieldMismatch>, usize) {
    let mut mismatches = Vec::new();
    let mut compared = 0usize;
    let Some(paths) = document.get("paths").and_then(|paths| paths.as_object()) else {
        return (mismatches, compared);
    };
    for (path, item) in paths {
        let Some(operations) = item.as_object() else {
            continue;
        };
        for (method, operation) in operations {
            let route = (method.to_uppercase(), path.clone());
            let Some(handler) = derived.handlers.get(&route) else {
                continue;
            };
            let Some(fields) = body_fields(handler) else {
                continue;
            };
            let schema = operation.pointer("/requestBody/content/application~1json/schema");
            let Some(schema) = schema else { continue };
            let declared = schema
                .get("properties")
                .and_then(|properties| properties.as_object());
            let Some(declared) = declared else { continue };
            compared += 1;
            let required: BTreeSet<&str> = schema
                .get("required")
                .and_then(|required| required.as_array())
                .into_iter()
                .flatten()
                .filter_map(|value| value.as_str())
                .collect();

            for (name, property) in declared {
                let Some(fact) = fields.get(name) else {
                    // Also an absence, so it names the fields that WERE read.
                    // If the list looks truncated the reader missed something,
                    // and that is visible here instead of costing a live field.
                    let read: Vec<&str> = fields.keys().map(String::as_str).collect();
                    mismatches.push(FieldMismatch {
                        operation: route.clone(),
                        field: name.clone(),
                        detail: format!(
                            "no `{name}` in the handler's body type (it reads: {})",
                            read.join(", ")
                        ),
                    });
                    continue;
                };
                // A numeric range the handler enforces and the schema does not
                // match: generation samples the DECLARED range, so every value
                // outside the enforced one is a guaranteed rejection.
                if let Some((low, high)) = fact.range {
                    let declared_low = property.get("minimum").and_then(|v| v.as_f64());
                    let declared_high = property.get("maximum").and_then(|v| v.as_f64());
                    let too_low = low.is_some_and(|low| declared_low.is_none_or(|d| d < low));
                    let too_high = high.is_some_and(|high| declared_high.is_none_or(|d| d > high));
                    if too_low || too_high {
                        mismatches.push(FieldMismatch {
                            operation: route.clone(),
                            field: name.clone(),
                            detail: format!(
                                "declared {}, but the handler accepts only {} ({})",
                                describe_bounds(declared_low, declared_high),
                                describe_bounds(low, high),
                                fact.evidence.as_deref().unwrap_or("read from source")
                            ),
                        });
                    }
                }
                // A closed value set the schema left open: every generated value
                // outside the set is a guaranteed rejection.
                if let Some(allowed) = &fact.allowed {
                    let declared_enum = property
                        .get("enum")
                        .and_then(|values| values.as_array())
                        .map(|values| {
                            values
                                .iter()
                                .filter_map(|value| value.as_str())
                                .map(str::to_string)
                                .collect::<Vec<_>>()
                        });
                    if declared_enum.as_ref() != Some(allowed) {
                        mismatches.push(FieldMismatch {
                            operation: route.clone(),
                            field: name.clone(),
                            // Name what the schema actually says. Reporting a
                            // declared 1..5 as "open" is its own small lie.
                            detail: {
                                let declared = match &declared_enum {
                                    Some(values) => format!("declared [{}]", values.join(", ")),
                                    None => {
                                        let low = property.get("minimum").and_then(|v| v.as_f64());
                                        let high = property.get("maximum").and_then(|v| v.as_f64());
                                        if low.is_some() || high.is_some() {
                                            format!("declared {}", describe_bounds(low, high))
                                        } else {
                                            "declared open".to_string()
                                        }
                                    }
                                };
                                format!(
                                    "{declared}, but the handler accepts only [{}] ({})",
                                    allowed.join(", "),
                                    fact.evidence.as_deref().unwrap_or("from its type")
                                )
                            },
                        });
                    }
                }
            }
            for (name, fact) in fields {
                if fact.required
                    && declared.contains_key(name.as_str())
                    && !required.contains(name.as_str())
                {
                    mismatches.push(FieldMismatch {
                        operation: route.clone(),
                        field: name.clone(),
                        detail: "the handler requires it, but the schema does not mark it \
                                 required, so it will be omitted and rejected"
                            .to_string(),
                    });
                }
            }
        }
    }
    mismatches.sort_by(|left, right| {
        (&left.operation, &left.field).cmp(&(&right.operation, &right.field))
    });
    (mismatches, compared)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn derived(routes: &[(&str, &[&'static str])]) -> Derived {
        let mut map: BTreeMap<String, BTreeSet<&'static str>> = BTreeMap::new();
        for (path, methods) in routes {
            map.insert(path.to_string(), methods.iter().copied().collect());
        }
        Derived {
            routes: map,
            files_scanned: 3,
            skipped: 0,
            handlers: BTreeMap::new(),
            unreadable: 0,
            bodies: BTreeMap::new(),
        }
    }

    fn route(method: &str, path: &str) -> Route {
        (method.to_string(), path.to_string())
    }

    #[test]
    fn a_matching_schema_is_clean() {
        let drift = diff(
            &[route("GET", "/users"), route("POST", "/users")],
            &derived(&[("/users", &["get", "post"])]),
        );
        assert!(drift.is_clean(), "{drift:?}");
        assert_eq!(drift.matched, 2);
        assert!(lines(&drift).is_empty());
    }

    #[test]
    fn a_declared_path_the_source_does_not_serve_is_reported() {
        // The expensive case: it 404s forever and reads as a passing operation.
        let drift = diff(
            &[route("GET", "/users"), route("GET", "/usres")],
            &derived(&[("/users", &["get"])]),
        );
        assert_eq!(drift.undeclared_by_source, vec![route("GET", "/usres")]);
        assert_eq!(drift.matched, 1);
        let report = lines(&drift).join("\n");
        assert!(report.contains("no route matched in source"), "{report}");
        assert!(report.contains("GET /usres"), "{report}");
    }

    #[test]
    fn a_served_route_missing_from_the_schema_is_reported() {
        let drift = diff(
            &[route("GET", "/users")],
            &derived(&[("/users", &["get"]), ("/admin/purge", &["post"])]),
        );
        assert_eq!(
            drift.unserved_by_schema,
            vec![route("POST", "/admin/purge")]
        );
        let report = lines(&drift).join("\n");
        assert!(
            report.contains("served by the source but not declared"),
            "{report}"
        );
        assert!(report.contains("POST /admin/purge"), "{report}");
    }

    #[test]
    fn a_path_parameter_named_differently_still_matches() {
        // The schema author writes {id}; the handler calls it {user_id}. Same
        // route, and reporting it as drift would train people to ignore this.
        let drift = diff(
            &[route("GET", "/users/{id}/posts/{post_id}")],
            &derived(&[("/users/{user_id}/posts/{pid}", &["get"])]),
        );
        assert!(drift.is_clean(), "{drift:?}");
    }

    #[test]
    fn method_case_does_not_matter() {
        let drift = diff(&[route("get", "/users")], &derived(&[("/users", &["get"])]));
        assert!(drift.is_clean(), "{drift:?}");
    }

    /// The body the handler actually takes, as the Rust parser reports it.
    fn block_fields() -> BTreeMap<String, field_facts::FieldFact> {
        BTreeMap::from([
            (
                "blocked_type".to_string(),
                field_facts::FieldFact {
                    required: true,
                    allowed: Some(vec!["user".into(), "sponsor".into()]),
                    range: None,
                    evidence: Some("a unit-only enum".into()),
                },
            ),
            (
                "note".to_string(),
                field_facts::FieldFact {
                    required: false,
                    ..field_facts::FieldFact::default()
                },
            ),
        ])
    }

    fn with_handler(method: &str, path: &str, handler: &str) -> Derived {
        let mut derived = derived(&[(path, &["post"])]);
        derived
            .handlers
            .insert((method.to_string(), path.to_string()), handler.to_string());
        derived
    }

    fn block_document(property: serde_json::Value, required: &[&str]) -> serde_json::Value {
        serde_json::json!({"paths": {"/block": {"post": {"requestBody": {"content": {
        "application/json": {"schema": {
            "type": "object",
            "required": required,
            "properties": {"blocked_type": property}
        }}}}}}}})
    }

    #[test]
    fn an_open_string_against_an_enum_handler_is_reported() {
        // The reported case: this cost 100% of that operation's mutations.
        let document = block_document(serde_json::json!({"type": "string"}), &["blocked_type"]);
        let found = compare_fields(
            &document,
            &with_handler("POST", "/block", "create_block"),
            &|handler| (handler == "create_block").then(block_fields),
        )
        .0;
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].field, "blocked_type");
        assert!(
            found[0].detail.contains("only [user, sponsor]"),
            "{}",
            found[0].detail
        );
    }

    #[test]
    fn a_matching_enum_declaration_is_silent() {
        let document = block_document(
            serde_json::json!({"type": "string", "enum": ["user", "sponsor"]}),
            &["blocked_type"],
        );
        let found = compare_fields(
            &document,
            &with_handler("POST", "/block", "create_block"),
            &|handler| (handler == "create_block").then(block_fields),
        )
        .0;
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn a_declared_field_the_handler_does_not_have_is_reported() {
        let document = serde_json::json!({"paths": {"/block": {"post": {"requestBody": {"content": {
            "application/json": {"schema": {"type": "object", "properties": {"blockedType": {"type": "string"}}}}}}}}}});
        let found = compare_fields(
            &document,
            &with_handler("POST", "/block", "create_block"),
            &|handler| (handler == "create_block").then(block_fields),
        )
        .0;
        assert_eq!(found.len(), 1, "{found:?}");
        // The message names the fields it DID read, so an incomplete parse is
        // visible here instead of costing someone a live field.
        assert!(found[0].detail.contains("no `blockedType`"), "{found:?}");
        assert!(
            found[0].detail.contains("it reads: blocked_type"),
            "it must list what it read: {found:?}"
        );
    }

    #[test]
    fn a_handler_required_field_the_schema_leaves_optional_is_reported() {
        // Generation omits it, the handler rejects the body, and the operation
        // reads as exercised while evaluating nothing.
        let document = block_document(
            serde_json::json!({"type": "string", "enum": ["user", "sponsor"]}),
            &[],
        );
        let found = compare_fields(
            &document,
            &with_handler("POST", "/block", "create_block"),
            &|handler| (handler == "create_block").then(block_fields),
        )
        .0;
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(
            found[0].detail.contains("does not mark it required"),
            "{found:?}"
        );
    }

    #[test]
    fn an_operation_with_no_resolvable_handler_abstains() {
        let document = block_document(serde_json::json!({"type": "string"}), &["blocked_type"]);
        let found = compare_fields(&document, &derived(&[("/block", &["post"])]), &|handler| {
            (handler == "create_block").then(block_fields)
        })
        .0;
        assert!(
            found.is_empty(),
            "an unresolved handler must not produce a verdict: {found:?}"
        );
    }

    #[test]
    fn a_method_the_source_does_not_serve_is_drift_even_on_a_served_path() {
        let drift = diff(
            &[route("GET", "/users"), route("DELETE", "/users")],
            &derived(&[("/users", &["get"])]),
        );
        assert_eq!(drift.undeclared_by_source, vec![route("DELETE", "/users")]);
    }
}
