//! Re-run operations that failed for want of a resource another operation makes.
//!
//! An operation on `/posts/{id}/going` cannot be evaluated with a generated
//! `id`: the service answers 404 and the contract is never exercised, so the run
//! reports the operation as reached and evaluates nothing. That is the single
//! largest remaining coverage loss, and it is not a bug in the oracles: the
//! request was simply never valid.
//!
//! The chain is drawn from STRUCTURE, never from names. A create is only used
//! for an operation whose path extends the created collection by exactly one
//! parameter segment (`POST /posts` -> `/posts/{id}/going`), and only from a
//! resource THIS RUN created. There is no singularisation, no `post_id` guessing,
//! no matching a field because it sounds like an identity. An operation whose
//! precondition cannot be established that way is left unevaluated and SAYS so,
//! because a fabricated precondition produces a finding about a request the
//! service was never asked to serve.

use super::*;

/// A precondition-backed confirmation: the setup that creates the resource, and
/// the request rebound to whatever identity that fresh create returns.
pub(super) struct Chained {
    pub(super) setup: Vec<ReplayStep>,
    pub(super) request: RequestArtifact,
}

/// The create whose resource satisfies `path`, and how to reach its identity.
///
/// `POST /posts` satisfies `/posts/{id}/going` because the path continues from
/// the created collection through exactly one parameter. `/posts/{id}/x/{y}` is
/// not satisfied: its second parameter has no established source, and guessing
/// one would be inventing a precondition.
fn precondition<'a>(
    path: &str,
    creates: &'a [CreateRecord],
) -> Option<(&'a CreateRecord, String, String, Value)> {
    creates.iter().find_map(|record| {
        let suffix = path.strip_prefix(record.endpoint.path.as_str())?;
        let rest = suffix.strip_prefix("/{")?;
        let (param, tail) = rest.split_once('}')?;
        if param.is_empty() || tail.contains('{') {
            return None;
        }
        let (identity_path, identity) = create_identity(&record.output, param)?;
        Some((record, param.to_string(), identity_path, identity))
    })
}

/// Make a finding on a dependent resource self-confirming.
///
/// A non-idempotent POST on `/posts/{id}/going` cannot be confirmed by re-sending
/// it: the resource is already in the acted-on state, so confirmation previously
/// needed a whole-service reset and, without one, a real violation was filed as
/// an unconfirmed candidate and never blocked anything.
///
/// It does not need a reset. The run already created the resource, so the same
/// sequence can be replayed against a FRESH one: create, rebind the identity the
/// create returns, act. That is self-contained, so it confirms here and keeps
/// confirming on replay, months later, on a machine that never saw this run.
pub(super) fn confirmable(
    endpoint: &Endpoint,
    request: &RequestArtifact,
    creates: &[CreateRecord],
) -> Option<Chained> {
    let (create, param, identity_path, _) = precondition(&endpoint.path, creates)?;
    let mut request = request.clone();
    // Bind to the create's output rather than the identity captured this run: a
    // replay must act on the resource IT created, not on a row that has since
    // been deleted.
    request.bindings.push(RequestBinding {
        source_step: 0,
        source_output_path: identity_path,
        input_path: format!("path.{param}"),
    });
    Some(Chained {
        setup: vec![ReplayStep {
            contract: create.endpoint.contract.clone(),
            request: create.request.clone(),
            policy: create.endpoint.policy.clone(),
        }],
        request,
    })
}

/// Confirm a violation on a dependent resource, or say why it could not be.
///
/// `None` means this operation has no create-able precondition, so the caller
/// falls through to its other confirmation routes. `Some(Err)` means there IS a
/// precondition and it did not confirm, which is a candidate rather than a
/// finding: a sequence that will not reproduce is not a bug we can prove.
pub(super) async fn confirm_dependent(
    client: &reqwest::Client,
    endpoint: &Endpoint,
    request: &RequestArtifact,
    creates: &[CreateRecord],
    violation: &BackendViolation,
) -> Option<Result<Chained, String>> {
    let chained = confirmable(endpoint, request, creates)?;
    let replay = replay_sequence(
        client,
        &chained.setup,
        endpoint,
        &chained.request,
        &violation.fingerprint,
        None,
        None,
    )
    .await;
    Some(match replay {
        Ok(ReplayVerdict::Reproduced) => Ok(chained),
        Ok(_) => {
            Err("replay against a freshly created resource did not reproduce exactly".to_string())
        }
        Err(error) => Err(format!("precondition replay failed: {error}")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create(path: &str, output: Value) -> CreateRecord {
        let endpoint = openapi_endpoints(&json!({
            "openapi": "3.0.3",
            "paths": {path: {"post": {"operationId": "create", "responses": {}}}}
        }))
        .pop()
        .expect("endpoint");
        CreateRecord {
            request: RequestArtifact {
                operation: "create".into(),
                method: "POST".into(),
                url: format!("http://127.0.0.1{path}"),
                input: Value::Null,
                headers: BTreeMap::new(),
                body: None,
                content_type: None,
                schema_source: None,
                client_streaming: false,
                server_streaming: false,
                bindings: Vec::new(),
            },
            endpoint,
            output,
        }
    }

    #[test]
    fn a_sub_resource_operation_takes_its_id_from_the_matching_create() {
        // The reported case: togglePostGoing 404s forever on a generated id.
        let creates = vec![create("/posts", json!({"id": "p-1"}))];
        let (_, param, identity_path, identity) =
            precondition("/posts/{id}/going", &creates).expect("matched");
        assert_eq!(param, "id");
        assert_eq!(identity_path, "id");
        assert_eq!(identity, json!("p-1"));
    }

    #[test]
    fn the_created_collection_itself_matches() {
        let creates = vec![create("/posts", json!({"id": "p-1"}))];
        assert!(precondition("/posts/{id}", &creates).is_some());
    }

    #[test]
    fn an_unrelated_collection_does_not_supply_an_identity() {
        // Structure, never names: /comments is not a source for /posts.
        let creates = vec![create("/comments", json!({"id": "c-1"}))];
        assert!(
            precondition("/posts/{id}/going", &creates).is_none(),
            "a create must not satisfy a path it does not prefix"
        );
    }

    #[test]
    fn a_second_unsourced_parameter_abstains() {
        // `/posts/{id}/x/{y}` has no established source for {y}, and inventing
        // one produces a finding about a request nobody asked for.
        let creates = vec![create("/posts", json!({"id": "p-1"}))];
        assert!(precondition("/posts/{id}/x/{y}", &creates).is_none());
    }

    #[test]
    fn a_create_with_no_scalar_identity_abstains() {
        let creates = vec![create("/posts", json!({"nested": {"deep": 1}}))];
        assert!(precondition("/posts/{id}", &creates).is_none());
    }
}
