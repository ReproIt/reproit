//! Deterministic probe planning for `reproit init` live enrichment.
//!
//! For each derived operation the plan synthesizes at most ONE bounded
//! request from what the source actually stated: path parameters get a
//! recorded synthetic value, mutating bodies are built only from field names
//! a reader parsed, and anything that cannot be synthesized honestly is
//! SKIPPED with the reason recorded rather than guessed at. Planning is pure;
//! sending lives in `enrich`.
//!
//! Safety rule, stated once and enforced here: mutating methods (POST, PUT,
//! PATCH, DELETE) are planned only when init booted the server itself and
//! tears it down afterwards. Against a server that was already running, the
//! plan holds every mutating probe back entirely, because that server and its
//! data belong to the user.

use super::extract::{path_params, Derived, METHODS};
use super::field_facts::FieldFact;

/// Synthetic value recorded for every path parameter. One deterministic
/// value, stated in the scaffold, so an observed 404 is attributable.
const PARAM_VALUE: &str = "1";
/// Synthetic value for a body field the source names but does not constrain.
const FIELD_VALUE: &str = "reproit";
/// Bound on synthesized body fields, mirroring the emitted shape bound.
const MAX_BODY_FIELDS: usize = 16;

/// One request the enrichment pass may send.
#[derive(Debug, PartialEq)]
pub(super) struct PlannedProbe {
    /// Lowercase method, from [`METHODS`].
    pub(super) method: &'static str,
    /// The derived path template this probe exercises.
    pub(super) path: String,
    /// The concrete request path, template params substituted.
    pub(super) request_path: String,
    /// Path parameter name -> the synthesized value, recorded in the draft.
    pub(super) params: Vec<(String, String)>,
    /// The synthesized JSON body, only for mutating methods with parsed fields.
    pub(super) body: Option<serde_json::Value>,
}

/// An operation the pass will NOT probe, and the honest reason why.
#[derive(Debug, PartialEq)]
pub(super) struct SkippedProbe {
    pub(super) method: &'static str,
    pub(super) path: String,
    pub(super) reason: &'static str,
}

pub(super) const SKIP_FOREIGN_SERVER: &str =
    "init never sends mutating requests to a server it did not boot itself";
pub(super) const SKIP_UNPARSED_BODY: &str =
    "request body fields not parseable from source, so no honest request exists";

#[derive(Debug, Default)]
pub(super) struct ProbePlan {
    pub(super) probes: Vec<PlannedProbe>,
    pub(super) skipped: Vec<SkippedProbe>,
}

impl ProbePlan {
    pub(super) fn skip_reason(&self, method: &str, path: &str) -> Option<&'static str> {
        self.skipped
            .iter()
            .find(|skip| skip.method == method && skip.path == path)
            .map(|skip| skip.reason)
    }

    pub(super) fn probe_for(&self, method: &str, path: &str) -> Option<&PlannedProbe> {
        self.probes
            .iter()
            .find(|probe| probe.method == method && probe.path == path)
    }
}

/// Plan one probe per derived operation, bounded by `cap` requests total.
///
/// GET is always planned. HEAD and OPTIONS are derived but not probed: they
/// add no response shape a GET does not, and one request per route is the
/// budget. Mutating methods are planned only when `mutations_allowed` (init
/// booted the server itself) AND, for body-carrying methods, the source
/// reader parsed the field names; each refusal is recorded, never silent.
pub(super) fn plan(derived: &Derived, mutations_allowed: bool, cap: usize) -> ProbePlan {
    let mut plan = ProbePlan::default();
    for (path, methods) in &derived.routes {
        for method in METHODS.iter().filter(|known| methods.contains(*known)) {
            match *method {
                "head" | "options" => continue,
                "get" => {}
                _ if !mutations_allowed => {
                    plan.skipped.push(SkippedProbe {
                        method,
                        path: path.clone(),
                        reason: SKIP_FOREIGN_SERVER,
                    });
                    continue;
                }
                _ => {}
            }
            let body = match *method {
                "post" | "put" | "patch" => match synthesize_body(derived, method, path) {
                    Some(body) => Some(body),
                    None => {
                        plan.skipped.push(SkippedProbe {
                            method,
                            path: path.clone(),
                            reason: SKIP_UNPARSED_BODY,
                        });
                        continue;
                    }
                },
                _ => None,
            };
            if plan.probes.len() >= cap {
                return plan;
            }
            let params: Vec<(String, String)> = path_params(path)
                .into_iter()
                .map(|name| (name.to_string(), PARAM_VALUE.to_string()))
                .collect();
            let mut request_path = path.clone();
            for (name, value) in &params {
                request_path = request_path.replace(&format!("{{{name}}}"), value);
            }
            plan.probes.push(PlannedProbe {
                method,
                path: path.clone(),
                request_path,
                params,
                body,
            });
        }
    }
    plan
}

/// A minimal body from the fields the source reader parsed for this route's
/// handler, or None when no field names are known. Required fields alone when
/// any field is marked required; every parsed field otherwise, bounded.
fn synthesize_body(derived: &Derived, method: &str, path: &str) -> Option<serde_json::Value> {
    let handler = derived
        .handlers
        .get(&(method.to_uppercase(), path.to_string()))?;
    let fields = derived.bodies.get(handler)?;
    if fields.is_empty() {
        return None;
    }
    let any_required = fields.values().any(|fact| fact.required);
    let mut body = serde_json::Map::new();
    for (name, fact) in fields
        .iter()
        .filter(|(_, fact)| !any_required || fact.required)
        .take(MAX_BODY_FIELDS)
    {
        body.insert(name.clone(), field_value(fact));
    }
    Some(serde_json::Value::Object(body))
}

/// The most constrained value the parsed fact permits: a stated allowed
/// value, then a stated range bound, then a plain marker string.
fn field_value(fact: &FieldFact) -> serde_json::Value {
    if let Some(first) = fact.allowed.as_ref().and_then(|values| values.first()) {
        if let Ok(number) = first.parse::<f64>() {
            if let Some(number) = serde_json::Number::from_f64(number) {
                return serde_json::Value::Number(number);
            }
        }
        return serde_json::Value::String(first.clone());
    }
    if let Some((min, max)) = fact.range {
        if let Some(number) = min.or(max).and_then(serde_json::Number::from_f64) {
            return serde_json::Value::Number(number);
        }
    }
    serde_json::Value::String(FIELD_VALUE.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn derived(routes: &[(&str, &[&'static str])]) -> Derived {
        let mut derived = Derived::default();
        for (path, methods) in routes {
            let entry = derived.routes.entry(path.to_string()).or_default();
            for method in *methods {
                entry.insert(method);
            }
        }
        derived
    }

    fn with_body(
        mut derived: Derived,
        method: &str,
        path: &str,
        fields: &[(&str, FieldFact)],
    ) -> Derived {
        let handler = format!("{method} {path} handler");
        derived
            .handlers
            .insert((method.to_uppercase(), path.to_string()), handler.clone());
        let mut map = BTreeMap::new();
        for (name, fact) in fields {
            map.insert(name.to_string(), fact.clone());
        }
        derived.bodies.insert(handler, map);
        derived
    }

    #[test]
    fn a_path_param_gets_one_synthesized_value_recorded() {
        let plan = plan(&derived(&[("/items/{id}", &["get"])]), false, 32);
        assert_eq!(plan.probes.len(), 1);
        let probe = &plan.probes[0];
        assert_eq!(probe.request_path, "/items/1");
        assert_eq!(probe.params, vec![("id".to_string(), "1".to_string())]);
        assert!(plan.skipped.is_empty());
    }

    #[test]
    fn mutating_probes_are_held_back_from_a_server_init_did_not_boot() {
        let derived = with_body(
            derived(&[("/items", &["get", "post"])]),
            "post",
            "/items",
            &[("name", FieldFact::default())],
        );
        let plan = plan(&derived, false, 32);
        assert_eq!(plan.probes.len(), 1, "only the GET is planned");
        assert_eq!(plan.probes[0].method, "get");
        assert_eq!(
            plan.skip_reason("post", "/items"),
            Some(SKIP_FOREIGN_SERVER)
        );
    }

    #[test]
    fn a_post_without_parsed_fields_is_skipped_honestly_even_when_booted() {
        let plan = plan(&derived(&[("/items", &["post"])]), true, 32);
        assert!(plan.probes.is_empty());
        assert_eq!(plan.skip_reason("post", "/items"), Some(SKIP_UNPARSED_BODY));
    }

    #[test]
    fn a_post_with_parsed_fields_synthesizes_exactly_one_minimal_body() {
        let constrained = FieldFact {
            allowed: Some(vec!["small".to_string(), "large".to_string()]),
            ..FieldFact::default()
        };
        let ranged = FieldFact {
            range: Some((Some(2.0), Some(9.0))),
            ..FieldFact::default()
        };
        let derived = with_body(
            derived(&[("/items", &["post"])]),
            "post",
            "/items",
            &[
                ("name", FieldFact::default()),
                ("size", constrained),
                ("count", ranged),
            ],
        );
        let plan = plan(&derived, true, 32);
        assert_eq!(plan.probes.len(), 1);
        assert_eq!(
            plan.probes[0].body,
            Some(serde_json::json!({"count": 2.0, "name": "reproit", "size": "small"}))
        );
    }

    #[test]
    fn required_fields_narrow_the_synthesized_body_to_required_only() {
        let required = FieldFact {
            required: true,
            ..FieldFact::default()
        };
        let derived = with_body(
            derived(&[("/items", &["post"])]),
            "post",
            "/items",
            &[("name", required), ("note", FieldFact::default())],
        );
        let plan = plan(&derived, true, 32);
        assert_eq!(
            plan.probes[0].body,
            Some(serde_json::json!({"name": "reproit"}))
        );
    }

    #[test]
    fn the_plan_is_bounded_and_delete_needs_no_body() {
        let many: Vec<(String, Vec<&'static str>)> = (0..40)
            .map(|index| (format!("/r{index:02}"), vec!["get"]))
            .collect();
        let refs: Vec<(&str, &[&'static str])> = many
            .iter()
            .map(|(path, methods)| (path.as_str(), methods.as_slice()))
            .collect();
        let plan_capped = plan(&derived(&refs), true, 32);
        assert_eq!(plan_capped.probes.len(), 32);

        let plan_delete = plan(&derived(&[("/items/{id}", &["delete"])]), true, 32);
        assert_eq!(plan_delete.probes.len(), 1);
        assert_eq!(plan_delete.probes[0].body, None);
        assert_eq!(plan_delete.probes[0].request_path, "/items/1");
    }

    #[test]
    fn head_and_options_are_neither_probed_nor_reported_skipped() {
        let plan = plan(
            &derived(&[("/items", &["head", "options", "get"])]),
            true,
            32,
        );
        assert_eq!(plan.probes.len(), 1);
        assert!(plan.skipped.is_empty());
    }
}
