use super::*;

pub(super) fn evaluate_authored_invariants(
    config: &BackendConfig,
    contract: &OperationContract,
    start: &BackendEvent,
    returned: &ReturnEvent<'_>,
    violations: &mut Vec<BackendViolation>,
) {
    let input = match &start.event {
        BackendEventKind::Start { input } => input,
        _ => return,
    };
    for invariant in &config.invariants {
        let (operation, reason) = match invariant {
            BackendInvariant::Range {
                operation,
                path,
                min,
                max,
            } => {
                let reason = json_path(returned.output, path).and_then(|value| {
                    let value = value.as_f64()?;
                    (min.is_some_and(|minimum| value < minimum)
                        || max.is_some_and(|maximum| value > maximum))
                    .then(|| format!("output {path} value {value} is outside the authored range"))
                });
                (operation, reason)
            }
            BackendInvariant::EqualsInput {
                operation,
                output_path,
                input_path,
            } => {
                let output = json_path(returned.output, output_path);
                let input = json_path(input, input_path);
                let reason = (output.is_some() && input.is_some() && output != input)
                    .then(|| format!("output {output_path} does not equal input {input_path}"));
                (operation, reason)
            }
            BackendInvariant::Unique { operation, path } => {
                let values = json_path_values(returned.output, path);
                let unique = values
                    .iter()
                    .map(|value| canonical_json(value))
                    .collect::<BTreeSet<_>>()
                    .len();
                let reason = (values.len() > 1 && unique != values.len())
                    .then(|| format!("output {path} contains duplicate values"));
                (operation, reason)
            }
            BackendInvariant::Idempotent { operation } => (operation, None),
            BackendInvariant::QuerySemantics {
                operation,
                items_path,
                filters,
                sort,
                limit_input_path,
                ..
            } => {
                let reason = (contract.authority != Authority::Inferred)
                    .then(|| {
                        query_response_violation(
                            input,
                            returned.output,
                            items_path,
                            filters,
                            sort.as_ref(),
                            limit_input_path.as_deref(),
                        )
                    })
                    .flatten();
                (operation, reason)
            }
            BackendInvariant::Conserved {
                operation,
                left_path,
                right_path,
            } => {
                let left = json_path(returned.output, left_path).and_then(Value::as_f64);
                let right = json_path(returned.output, right_path).and_then(Value::as_f64);
                let reason = left.zip(right).and_then(|(left, right)| {
                    ((left - right).abs() > f64::EPSILON).then(|| {
                        format!(
                            "{left_path} value {left} is not conserved with {right_path} value \
                             {right}"
                        )
                    })
                });
                (operation, reason)
            }
            BackendInvariant::Bounded {
                operation,
                value_path,
                limit_path,
            } => {
                let value = json_path(returned.output, value_path).and_then(Value::as_f64);
                let limit = json_path(returned.output, limit_path).and_then(Value::as_f64);
                let reason = value.zip(limit).and_then(|(value, limit)| {
                    (value > limit).then(|| {
                        format!("{value_path} value {value} exceeds {limit_path} value {limit}")
                    })
                });
                (operation, reason)
            }
            BackendInvariant::Transition {
                operation,
                path,
                from,
                to,
            } => {
                let (before, after) = if path == "*" {
                    paired_transition(input, returned.output, from)
                        .map(|after| (Some(from.as_str()), Some(after)))
                        .unwrap_or((None, None))
                } else {
                    (
                        json_path(input, path).and_then(Value::as_str),
                        json_path(returned.output, path).and_then(Value::as_str),
                    )
                };
                let reason = (before == Some(from.as_str()))
                    .then_some(after)
                    .flatten()
                    .and_then(|after| {
                        (!to.iter().any(|allowed| allowed == after)).then(|| {
                            format!(
                                "transition {from} -> {after} is outside the authored targets \
                                 {to:?}"
                            )
                        })
                    });
                (operation, reason)
            }
        };
        if operation == "*" || operation == &contract.id {
            if let Some(reason) = reason {
                violations.push(violation(
                    contract,
                    returned.event,
                    "authored-invariant",
                    reason,
                ));
            }
        }
    }
}

fn query_response_violation(
    input: &Value,
    output: &Value,
    items_path: &str,
    filters: &[QueryFilterContract],
    sort: Option<&QuerySortContract>,
    limit_input_path: Option<&str>,
) -> Option<String> {
    let items = json_path(output, items_path)?.as_array()?;
    for filter in filters {
        let expected = scalar_at(input, &filter.input_path)?;
        for item in items {
            let actual = scalar_at(item, &filter.item_path)?;
            if filter.comparison == QueryComparison::Equal && actual != expected {
                return Some(format!(
                    "query equality filter {} returned an item that contradicted {}",
                    filter.input_path, filter.item_path
                ));
            }
        }
    }
    if let Some(sort) = sort {
        let values = items
            .iter()
            .map(|item| scalar_at(item, &sort.item_path))
            .collect::<Option<Vec<_>>>()?;
        for pair in values.windows(2) {
            let ordering = typed_query_cmp(pair[0], pair[1], sort.value_type)?;
            let valid = match sort.direction {
                QuerySortDirection::Ascending => ordering != std::cmp::Ordering::Greater,
                QuerySortDirection::Descending => ordering != std::cmp::Ordering::Less,
            };
            if !valid {
                return Some(format!(
                    "query sort {} contradicted the authored {:?} {:?} order",
                    sort.item_path, sort.value_type, sort.direction
                ));
            }
        }
    }
    if let Some(path) = limit_input_path {
        let limit = json_path(input, path)?.as_u64()?;
        if items.len() as u64 > limit {
            return Some(format!(
                "query result count exceeded the authored limit at {path}"
            ));
        }
    }
    None
}

fn typed_query_cmp(left: &Value, right: &Value, kind: QuerySortType) -> Option<std::cmp::Ordering> {
    match kind {
        QuerySortType::String => left.as_str()?.partial_cmp(right.as_str()?),
        QuerySortType::Number => exact_json_number_cmp(left, right),
        QuerySortType::Boolean => left.as_bool()?.partial_cmp(&right.as_bool()?),
    }
}

fn exact_json_number_cmp(left: &Value, right: &Value) -> Option<std::cmp::Ordering> {
    let left = left.as_number()?;
    let right = right.as_number()?;
    match (left.as_i64(), left.as_u64(), right.as_i64(), right.as_u64()) {
        (Some(left), _, Some(right), _) => Some(left.cmp(&right)),
        (Some(left), _, None, Some(right)) => Some(if left < 0 {
            std::cmp::Ordering::Less
        } else {
            (left as u64).cmp(&right)
        }),
        (None, Some(left), Some(right), _) => Some(if right < 0 {
            std::cmp::Ordering::Greater
        } else {
            left.cmp(&(right as u64))
        }),
        (None, Some(left), None, Some(right)) => Some(left.cmp(&right)),
        _ => left.as_f64()?.partial_cmp(&right.as_f64()?),
    }
}

pub(super) fn evaluate_query_pagination(
    config: &BackendConfig,
    contracts: &BTreeMap<&str, &OperationContract>,
    invocations: &BTreeMap<(String, String), Invocation<'_>>,
    violations: &mut Vec<BackendViolation>,
) {
    for invariant in &config.invariants {
        let BackendInvariant::QuerySemantics {
            operation,
            items_path,
            identity_path,
            consistency,
            pagination: Some(pagination),
            ..
        } = invariant
        else {
            continue;
        };
        let Some(contract) = contracts.get(operation.as_str()) else {
            continue;
        };
        if contract.authority == Authority::Inferred || *consistency != ResourceConsistency::Strong
        {
            continue;
        }
        let mut pages = invocations
            .values()
            .filter(|invocation| {
                invocation
                    .start
                    .is_some_and(|start| start.operation == *operation)
                    && invocation
                        .returned
                        .as_ref()
                        .is_some_and(|returned| contract.is_success(returned))
            })
            .collect::<Vec<_>>();
        pages.sort_by_key(|invocation| invocation.start.map(|start| start.sequence));
        if pages.len() < 2 {
            continue;
        }

        let Some(snapshot) = common_query_scalar_input(&pages, &pagination.snapshot_input_path)
        else {
            continue;
        };
        let actor = pages[0].start.and_then(|start| start.actor.as_deref());
        let tenant = pages[0].start.and_then(|start| start.tenant.as_deref());
        if pages.iter().any(|page| {
            page.start.and_then(|start| start.actor.as_deref()) != actor
                || page.start.and_then(|start| start.tenant.as_deref()) != tenant
        }) {
            continue;
        }
        let Some(base_input) = pages[0].start.and_then(|start| match &start.event {
            BackendEventKind::Start { input } => {
                query_input_without_cursor(input, &pagination.cursor_input_path)
            }
            _ => None,
        }) else {
            continue;
        };
        if pages.iter().any(|page| {
            page.start
                .and_then(|start| match &start.event {
                    BackendEventKind::Start { input } => {
                        query_input_without_cursor(input, &pagination.cursor_input_path)
                    }
                    _ => None,
                })
                .as_ref()
                != Some(&base_input)
        }) {
            // Multiple logical queries or sessions are present. Without an
            // explicit session identity, combining their pages is UNKNOWN.
            continue;
        }

        let mut expected_cursor: Option<String> = None;
        let mut seen_cursors = BTreeSet::new();
        let mut seen_ids = BTreeSet::new();
        let mut concatenated = Vec::new();
        let mut proof_event = None;
        let mut complete_chain = true;
        for (index, page) in pages.iter().enumerate() {
            let Some(start) = page.start else {
                complete_chain = false;
                break;
            };
            let BackendEventKind::Start { input } = &start.event else {
                complete_chain = false;
                break;
            };
            let Some(returned) = page.returned.as_ref() else {
                complete_chain = false;
                break;
            };
            let cursor = optional_query_scalar(input, &pagination.cursor_input_path);
            if index == 0 {
                if cursor.is_some() {
                    complete_chain = false;
                    break;
                }
            } else if cursor != expected_cursor {
                complete_chain = false;
                break;
            }
            let Some(items) = json_path(returned.output, items_path).and_then(Value::as_array)
            else {
                complete_chain = false;
                break;
            };
            for item in items {
                let Some(identity) = scalar_at(item, identity_path).map(canonical_json) else {
                    complete_chain = false;
                    break;
                };
                if !seen_ids.insert(identity.clone()) {
                    proof_event = Some((
                        returned.event,
                        format!(
                            "pinned pagination returned a duplicate identity at {identity_path} \
                             across pages"
                        ),
                    ));
                    break;
                }
                concatenated.push(item.clone());
            }
            if proof_event.is_some() || !complete_chain {
                break;
            }
            expected_cursor =
                optional_query_scalar(returned.output, &pagination.next_cursor_output_path);
            if let Some(next) = &expected_cursor {
                if !seen_cursors.insert(next.clone()) {
                    proof_event = Some((
                        returned.event,
                        format!(
                            "pinned pagination repeated a nonterminal cursor without progress at \
                             {}",
                            pagination.next_cursor_output_path
                        ),
                    ));
                    break;
                }
            } else if index + 1 != pages.len() {
                complete_chain = false;
                break;
            }
        }
        if let Some((event, reason)) = proof_event {
            violations.push(violation(contract, event, "query-pagination", reason));
            continue;
        }
        // An observation ending on a nonterminal cursor is incomplete, not a
        // pagination failure. Never infer nontermination from a bounded trace.
        if !complete_chain || expected_cursor.is_some() {
            continue;
        }
        let Some(reference_operation) = pagination.reference_operation.as_deref() else {
            continue;
        };
        let Some(reference_contract) = contracts.get(reference_operation) else {
            continue;
        };
        if reference_contract.authority == Authority::Inferred {
            continue;
        }
        let reference = invocations.values().find(|invocation| {
            invocation.start.is_some_and(|start| {
                start.operation == reference_operation
                    && start.actor.as_deref() == actor
                    && start.tenant.as_deref() == tenant
                    && match &start.event {
                        BackendEventKind::Start { input } => {
                            query_filter_inputs_match(&base_input, input, invariant)
                        }
                        _ => false,
                    }
                    && json_path(
                        match &start.event {
                            BackendEventKind::Start { input } => input,
                            _ => return false,
                        },
                        &pagination.snapshot_input_path,
                    ) == Some(snapshot)
            }) && invocation
                .returned
                .as_ref()
                .is_some_and(|returned| reference_contract.is_success(returned))
        });
        let Some(reference_items) = reference
            .and_then(|invocation| invocation.returned.as_ref())
            .and_then(|returned| json_path(returned.output, items_path))
            .and_then(Value::as_array)
        else {
            continue;
        };
        if &concatenated != reference_items {
            let event = pages
                .last()
                .and_then(|page| page.returned.as_ref())
                .map(|returned| returned.event)
                .expect("pages have successful returns");
            violations.push(violation(
                contract,
                event,
                "query-pagination-reference",
                "concatenated pinned pages differ from the declared reference operation".into(),
            ));
        }
    }
}

fn query_filter_inputs_match(
    page_input: &Value,
    reference_input: &Value,
    invariant: &BackendInvariant,
) -> bool {
    let BackendInvariant::QuerySemantics { filters, .. } = invariant else {
        return false;
    };
    filters.iter().all(|filter| {
        scalar_at(page_input, &filter.input_path)
            .zip(scalar_at(reference_input, &filter.input_path))
            .is_some_and(|(page, reference)| page == reference)
    })
}

fn query_input_without_cursor(input: &Value, path: &str) -> Option<Value> {
    let mut input = input.clone();
    let parts = path
        .trim_start_matches('$')
        .trim_start_matches('.')
        .split('.')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    if parts.is_empty() || !remove_json_path(&mut input, &parts) {
        return None;
    }
    Some(input)
}

fn remove_json_path(value: &mut Value, parts: &[&str]) -> bool {
    let Value::Object(object) = value else {
        return false;
    };
    if parts.len() == 1 {
        return object.remove(parts[0]).is_some();
    }
    object
        .get_mut(parts[0])
        .is_some_and(|next| remove_json_path(next, &parts[1..]))
}
