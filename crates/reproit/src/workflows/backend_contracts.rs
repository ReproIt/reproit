//! Read-only backend contract authoring report.
//!
//! Schema claims remain schema-scoped authority. Source and trace-derived
//! behavior is always a suggestion and cannot create findings.

use crate::domain::backend::{
    self, Authority, BackendConfig, EffectKind, EffectPattern, OperationContract,
};
use anyhow::Result;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SuggestionReport {
    command: &'static str,
    operations: Vec<OperationCoverage>,
    suggestions: Vec<ContractSuggestion>,
    abstentions: Vec<Abstention>,
    summary: CoverageSummary,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct OperationCoverage {
    operation: String,
    authority: Authority,
    authoritative_for_findings: bool,
    schema: bool,
    declared: bool,
    inferred_source: bool,
    lifecycle_roles: Vec<&'static str>,
    proof_contracts: usize,
    promised_effects: usize,
    inferred_effects: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ContractSuggestion {
    operation: String,
    authority: Authority,
    authoritative_for_findings: bool,
    basis: String,
    proposed_promised_effects: Vec<EffectPattern>,
    next_action: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Abstention {
    operation: String,
    code: &'static str,
    detail: String,
    missing_capability: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CoverageSummary {
    operations: usize,
    declared: usize,
    schema_only: usize,
    inferred_only: usize,
    lifecycle_covered: usize,
    proof_covered: usize,
    abstentions: usize,
}

pub(super) fn suggestion_report(root: &Path, config: &BackendConfig) -> Result<serde_json::Value> {
    let (schema_operations, duplicate_schema_ids) = load_schema_operations(root, config)?;
    let declared = operation_map(&config.operations);
    let schema = operation_map(&schema_operations);
    let inferred = inferred_operations(config);
    let lifecycle_roles = lifecycle_roles(config);
    let proof_counts = proof_counts(config);
    let operation_ids = declared
        .keys()
        .chain(schema.keys())
        .chain(inferred.keys())
        .copied()
        .collect::<BTreeSet<_>>();

    let mut operations = Vec::with_capacity(operation_ids.len());
    let mut suggestions = Vec::new();
    let mut abstentions = duplicate_abstentions(duplicate_schema_ids);
    for operation_id in operation_ids {
        let (coverage, operation_suggestions, operation_abstentions) = cover_operation(
            operation_id,
            declared.get(operation_id).copied(),
            schema.get(operation_id).copied(),
            inferred.get(operation_id).cloned().unwrap_or_default(),
            inferred.contains_key(operation_id),
            lifecycle_roles
                .get(operation_id)
                .cloned()
                .unwrap_or_default(),
            proof_counts.get(operation_id).copied().unwrap_or(0),
        );
        operations.push(coverage);
        suggestions.extend(operation_suggestions);
        abstentions.extend(operation_abstentions);
    }
    let summary = summarize(&operations, abstentions.len());
    serde_json::to_value(SuggestionReport {
        command: "debug map suggest-contracts",
        operations,
        suggestions,
        abstentions,
        summary,
    })
    .map_err(Into::into)
}

fn cover_operation(
    operation: &str,
    declared: Option<&OperationContract>,
    schema: Option<&OperationContract>,
    inferred_effects: Vec<EffectPattern>,
    inferred_source: bool,
    lifecycle_roles: Vec<&'static str>,
    proof_contracts: usize,
) -> (OperationCoverage, Vec<ContractSuggestion>, Vec<Abstention>) {
    let mut suggestions = Vec::new();
    let mut abstentions = Vec::new();
    add_suggestions(
        &mut suggestions,
        operation,
        declared,
        schema,
        &inferred_effects,
    );
    add_abstentions(
        &mut abstentions,
        operation,
        declared,
        schema,
        &inferred_effects,
        &lifecycle_roles,
    );
    let authority = declared
        .map(|contract| contract.authority)
        .or_else(|| schema.map(|contract| contract.authority))
        .unwrap_or(Authority::Inferred);
    let coverage = OperationCoverage {
        operation: operation.to_string(),
        authority,
        authoritative_for_findings: authority != Authority::Inferred,
        schema: schema.is_some(),
        declared: declared.is_some_and(|contract| contract.authority == Authority::Declared),
        inferred_source,
        lifecycle_roles,
        proof_contracts,
        promised_effects: declared
            .or(schema)
            .map_or(0, |contract| contract.promised_effects.len()),
        inferred_effects: inferred_effects.len(),
    };
    (coverage, suggestions, abstentions)
}

fn load_schema_operations(
    root: &Path,
    config: &BackendConfig,
) -> Result<(Vec<OperationContract>, BTreeSet<String>)> {
    let mut operations = Vec::new();
    let mut seen = BTreeSet::new();
    let mut duplicates = BTreeSet::new();
    for relative in &config.schemas {
        let document = backend::load_service_document(&root.join(relative))?;
        for operation in backend::import_service_schema(&document) {
            if seen.insert(operation.id.clone()) {
                operations.push(operation);
            } else {
                duplicates.insert(operation.id);
            }
        }
    }
    Ok((operations, duplicates))
}

fn operation_map(operations: &[OperationContract]) -> BTreeMap<&str, &OperationContract> {
    operations
        .iter()
        .map(|operation| (operation.id.as_str(), operation))
        .collect()
}

fn inferred_operations(config: &BackendConfig) -> BTreeMap<&str, Vec<EffectPattern>> {
    let mut operations = BTreeMap::<&str, Vec<EffectPattern>>::new();
    for program in &config.programs {
        for function in &program.functions {
            let Some(operation) = function.operation.as_deref() else {
                continue;
            };
            let effects = operations.entry(operation).or_default();
            for effect in &function.effects {
                let pattern = EffectPattern {
                    kind: effect.kind,
                    resource: effect.resource.clone(),
                    event: effect.event.clone(),
                    at_least: 1,
                    at_most: None,
                };
                if !effects.contains(&pattern) {
                    effects.push(pattern);
                }
            }
        }
    }
    operations
}

fn lifecycle_roles(config: &BackendConfig) -> BTreeMap<&str, Vec<&'static str>> {
    let mut roles = BTreeMap::<&str, Vec<&'static str>>::new();
    for resource in &config.resources {
        roles
            .entry(&resource.create.operation)
            .or_default()
            .push("create");
        roles
            .entry(&resource.read.operation)
            .or_default()
            .push("read");
        if let Some(update) = &resource.update {
            roles.entry(&update.operation).or_default().push("update");
        }
        if let Some(delete) = &resource.delete {
            roles.entry(&delete.operation).or_default().push("delete");
        }
    }
    roles
}

fn proof_counts(config: &BackendConfig) -> BTreeMap<&str, usize> {
    let mut counts = BTreeMap::new();
    for proof in &config.proofs {
        for operation in proof.operation_ids() {
            *counts.entry(operation).or_default() += 1;
        }
    }
    counts
}

fn add_suggestions(
    suggestions: &mut Vec<ContractSuggestion>,
    operation: &str,
    declared: Option<&OperationContract>,
    schema: Option<&OperationContract>,
    inferred_effects: &[EffectPattern],
) {
    if declared.is_none() {
        if let Some(schema) = schema {
            suggestions.push(ContractSuggestion {
                operation: operation.into(),
                authority: Authority::Schema,
                authoritative_for_findings: true,
                basis: format!(
                    "schema declares {} success status(es) and {} response shape(s)",
                    schema.success_statuses.len(),
                    schema.outputs_by_status.len()
                ),
                proposed_promised_effects: Vec::new(),
                next_action: "review business semantics before promoting them to declared",
            });
        }
    }
    if !inferred_effects.is_empty() {
        suggestions.push(ContractSuggestion {
            operation: operation.into(),
            authority: Authority::Inferred,
            authoritative_for_findings: false,
            basis: "static source summary observed effect boundaries".into(),
            proposed_promised_effects: inferred_effects.to_vec(),
            next_action: "an application owner must review and declare these effects",
        });
    }
}

fn add_abstentions(
    abstentions: &mut Vec<Abstention>,
    operation: &str,
    declared: Option<&OperationContract>,
    schema: Option<&OperationContract>,
    inferred_effects: &[EffectPattern],
    lifecycle_roles: &[&str],
) {
    if declared.is_none() && schema.is_none() {
        abstentions.push(Abstention {
            operation: operation.into(),
            code: "inferred-operation-only",
            detail: "source inference is suggestive and cannot create a finding".into(),
            missing_capability: "schema or application-authored operation contract",
        });
    }
    let mutates = declared
        .into_iter()
        .flat_map(|contract| &contract.promised_effects)
        .chain(inferred_effects)
        .any(|effect| matches!(effect.kind, EffectKind::Write | EffectKind::Delete));
    if mutates && lifecycle_roles.is_empty() {
        abstentions.push(Abstention {
            operation: operation.into(),
            code: "effect-without-lifecycle",
            detail: "mutation evidence has no declared resource identity lifecycle".into(),
            missing_capability: "backend.resources lifecycle contract",
        });
    }
}

fn duplicate_abstentions(duplicates: BTreeSet<String>) -> Vec<Abstention> {
    duplicates
        .into_iter()
        .map(|operation| Abstention {
            operation,
            code: "duplicate-schema-operation",
            detail: "operationId appears in more than one schema".into(),
            missing_capability: "unique operationId across backend.schemas",
        })
        .collect()
}

fn summarize(operations: &[OperationCoverage], abstentions: usize) -> CoverageSummary {
    CoverageSummary {
        operations: operations.len(),
        declared: operations
            .iter()
            .filter(|operation| operation.declared)
            .count(),
        schema_only: operations
            .iter()
            .filter(|operation| operation.schema && !operation.declared)
            .count(),
        inferred_only: operations
            .iter()
            .filter(|operation| !operation.schema && !operation.declared)
            .count(),
        lifecycle_covered: operations
            .iter()
            .filter(|operation| !operation.lifecycle_roles.is_empty())
            .count(),
        proof_covered: operations
            .iter()
            .filter(|operation| operation.proof_contracts > 0)
            .count(),
        abstentions,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temporary_root() -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "reproit-backend-contract-report-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn schema_authority_and_inferred_effects_remain_visibly_distinct() {
        let root = temporary_root();
        std::fs::write(
            root.join("openapi.yaml"),
            r#"openapi: 3.1.0
paths:
  /orders:
    post:
      operationId: createOrder
      responses:
        '201':
          description: created
"#,
        )
        .unwrap();
        let config: BackendConfig = serde_yaml::from_str(
            r#"enabled: true
schemas: [openapi.yaml]
programs:
  - language: rust
    functions:
      - id: create-order
        name: create_order
        operation: createOrder
        effects:
          - kind: write
            resource: orders
"#,
        )
        .unwrap();

        let report = suggestion_report(&root, &config).unwrap();
        assert_eq!(report["operations"][0]["authority"], "schema");
        assert_eq!(report["operations"][0]["authoritativeForFindings"], true);
        assert_eq!(report["suggestions"][1]["authority"], "inferred");
        assert_eq!(report["suggestions"][1]["authoritativeForFindings"], false);
        assert!(report["abstentions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| {
                item["code"] == "effect-without-lifecycle"
                    && item["missingCapability"] == "backend.resources lifecycle contract"
            }));
        std::fs::remove_dir_all(root).unwrap();
    }
}
