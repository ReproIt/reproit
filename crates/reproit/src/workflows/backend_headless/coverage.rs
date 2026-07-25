use super::*;

/// Per-operation reach for one run.
///
/// `12 operations, 36 exercised, 0 findings` cannot tell a clean sweep apart
/// from one that 400'd on every mutation and never evaluated a single contract:
/// the aggregate implies coverage the run does not have. For a tool whose claim
/// is "finds what your tests miss", being unable to say what it did not touch is
/// the sharpest possible hole. So the run records what each DECLARED operation
/// actually did, and the report leads with what was never reached. Same
/// principle as the inconclusive verdict, one level down.
pub(super) struct Coverage {
    operations: BTreeMap<String, OperationReach>,
}

#[derive(Default)]
struct OperationReach {
    method: String,
    attempts: usize,
    ok: usize,
    client_error: usize,
    rate_limited: usize,
    server_error: usize,
    transport_errors: usize,
    last_status: Option<u16>,
    /// The last non-2xx body, trimmed. Usually the single most useful field on
    /// the whole report: when an operation 400s every attempt it normally names
    /// the input the declared schema got wrong.
    last_body: Option<String>,
    /// Why the operation was never sent, when it was not.
    not_sent: Option<String>,
}

/// Bodies are evidence, not logs: enough to name the rejected field, bounded so
/// one chatty error cannot swamp the report.
const MAX_BODY_SNIPPET: usize = 200;

impl Coverage {
    /// Seed with every declared operation, so one that is never sent still has a
    /// row. An operation missing from the report is the case this exists to make
    /// impossible.
    pub(super) fn new(endpoints: &[Endpoint]) -> Self {
        let mut operations = BTreeMap::new();
        for endpoint in endpoints {
            operations.insert(
                endpoint.contract.id.clone(),
                OperationReach {
                    method: endpoint.method.clone(),
                    ..OperationReach::default()
                },
            );
        }
        Self { operations }
    }

    fn entry(&mut self, operation: &str) -> &mut OperationReach {
        self.operations.entry(operation.to_string()).or_default()
    }

    pub(super) fn record(&mut self, operation: &str, status: u16, body: &Value) {
        let reach = self.entry(operation);
        reach.attempts += 1;
        reach.last_status = Some(status);
        match status {
            200..=399 => reach.ok += 1,
            429 => reach.rate_limited += 1,
            400..=499 => reach.client_error += 1,
            500..=599 => reach.server_error += 1,
            _ => {}
        }
        if !(200..400).contains(&status) {
            reach.last_body = body_snippet(body);
        }
    }

    pub(super) fn not_sent(&mut self, operation: &str, reason: &str) {
        let reach = self.entry(operation);
        if reach.not_sent.is_none() {
            reach.not_sent = Some(reason.to_string());
        }
    }

    pub(super) fn transport_error(&mut self, operation: &str) {
        self.entry(operation).transport_errors += 1;
    }

    pub(super) fn evaluated_count(&self) -> usize {
        self.operations
            .values()
            .filter(|reach| reach.ok > 0)
            .count()
    }

    pub(super) fn report(&self) -> Vec<Value> {
        self.operations
            .iter()
            .map(|(id, reach)| {
                let mut row = json!({
                    "operation": id,
                    "method": reach.method,
                    "attempts": reach.attempts,
                    "ok": reach.ok,
                    "clientError": reach.client_error,
                    "rateLimited": reach.rate_limited,
                    "serverError": reach.server_error,
                    "transportErrors": reach.transport_errors,
                    // The two questions the aggregate could not answer.
                    "reached": reach.attempts > 0,
                    "evaluated": reach.ok > 0,
                });
                if let Some(status) = reach.last_status {
                    row["lastStatus"] = json!(status);
                }
                if let Some(body) = &reach.last_body {
                    row["lastBody"] = json!(body);
                }
                if let Some(reason) = &reach.not_sent {
                    row["notSentReason"] = json!(reason);
                }
                row
            })
            .collect()
    }
}

/// A single-line, bounded rendering of a response body.
fn body_snippet(body: &Value) -> Option<String> {
    let text = match body {
        Value::Null => return None,
        Value::String(text) => text.clone(),
        other => other.to_string(),
    };
    let flattened = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flattened.is_empty() {
        return None;
    }
    Some(if flattened.chars().count() > MAX_BODY_SNIPPET {
        let kept: String = flattened.chars().take(MAX_BODY_SNIPPET).collect();
        format!("{kept}...")
    } else {
        flattened
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn endpoint(id: &str, method: &str) -> Endpoint {
        let mut endpoint = openapi_endpoints(&json!({
            "openapi": "3.0.3",
            "paths": {"/x": {"get": {"operationId": id, "responses": {}}}}
        }))
        .pop()
        .expect("one endpoint");
        endpoint.method = method.to_string();
        endpoint
    }

    #[test]
    fn a_declared_operation_that_is_never_sent_still_has_a_row() {
        // The whole point: an operation missing from the report would let the
        // aggregate imply coverage the run never had.
        let coverage = Coverage::new(&[endpoint("blockUser", "POST")]);
        let report = coverage.report();
        assert_eq!(report.len(), 1);
        assert_eq!(report[0]["reached"], json!(false));
        assert_eq!(report[0]["evaluated"], json!(false));
        assert_eq!(report[0]["attempts"], json!(0));
    }

    #[test]
    fn an_operation_that_only_ever_4xxs_is_reached_but_not_evaluated() {
        let mut coverage = Coverage::new(&[endpoint("blockUser", "POST")]);
        for _ in 0..3 {
            coverage.record(
                "blockUser",
                400,
                &json!({"error": "blocked_type must be one of user, sponsor"}),
            );
        }
        let row = &coverage.report()[0];
        assert_eq!(row["reached"], json!(true));
        assert_eq!(
            row["evaluated"],
            json!(false),
            "a 400 evaluates no contract"
        );
        assert_eq!(row["clientError"], json!(3));
        assert_eq!(row["lastStatus"], json!(400));
        assert!(row["lastBody"]
            .as_str()
            .unwrap()
            .contains("blocked_type must be one of"));
        assert_eq!(coverage.evaluated_count(), 0);
    }

    #[test]
    fn rate_limiting_is_counted_apart_from_other_client_errors() {
        let mut coverage = Coverage::new(&[endpoint("listNearby", "GET")]);
        coverage.record("listNearby", 429, &Value::Null);
        coverage.record("listNearby", 429, &Value::Null);
        let row = &coverage.report()[0];
        assert_eq!(row["rateLimited"], json!(2));
        assert_eq!(row["clientError"], json!(0));
    }

    #[test]
    fn body_snippets_are_single_line_and_bounded() {
        let long = body_snippet(&Value::String("x\n y\t z".repeat(200))).unwrap();
        assert!(
            long.chars().count() <= MAX_BODY_SNIPPET + 3,
            "{}",
            long.len()
        );
        assert!(!long.contains('\n'));
        assert_eq!(body_snippet(&Value::Null), None);
    }
}
