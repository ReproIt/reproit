use super::{EffectPattern, OperationContract, ProgramSummary};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BackendConfig {
    #[serde(default)]
    pub enabled: bool,
    /// Additional trusted API origins that may receive per-action correlation
    /// headers. The application origin is always included by the web runner.
    #[serde(default)]
    pub origins: Vec<String>,
    #[serde(default)]
    pub schemas: Vec<String>,
    #[serde(default)]
    pub operations: Vec<OperationContract>,
    /// Compiler and language adapters write normalized function summaries
    /// here. They guide generation and slicing but never create findings alone.
    #[serde(default)]
    pub programs: Vec<ProgramSummary>,
    /// Business invariants are opt-in. They are evaluated only against a
    /// successful runtime witness, so inferred code facts never become alerts.
    #[serde(default)]
    pub invariants: Vec<BackendInvariant>,
    /// Explicit cross-operation resource contracts. Lifecycle findings require
    /// a strong-consistency declaration and a complete, correlated runtime
    /// witness. Missing or ambiguous identity evidence always abstains.
    #[serde(default)]
    pub resources: Vec<ResourceLifecycleContract>,
    /// Authored cross-request proof contracts. These never infer semantics from
    /// framework, route, field, or status names.
    #[serde(default)]
    pub proofs: Vec<BackendProofContract>,
    #[serde(default)]
    pub fleet: FleetInvariant,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ResourceConsistency {
    #[default]
    Unspecified,
    Strong,
    Eventual,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum BackendProofContract {
    AuthorizationMatrix {
        operation: String,
        identity_input_path: String,
        snapshot_input_path: String,
        consistency: ResourceConsistency,
        principals: Vec<AuthorizationPrincipal>,
        deny: AuthorizationDenyPolicy,
    },
    TransactionAtomicity {
        operation: String,
        identity_input_path: String,
        snapshot_input_path: String,
        consistency: ResourceConsistency,
        failure: ControlledFailureWitness,
        durable_effects: Vec<EffectPattern>,
    },
    ConcurrentUpdate {
        operation: String,
        identity_input_path: String,
        snapshot_input_path: String,
        consistency: ResourceConsistency,
        policy: ConcurrencyPolicy,
    },
    ResourceRoundTrip {
        write_operation: String,
        read_operation: String,
        write_identity_output_path: String,
        read_identity_input_path: String,
        write_snapshot_output_path: String,
        read_snapshot_input_path: String,
        consistency: ResourceConsistency,
        checks: Vec<RoundTripCheck>,
    },
}

impl BackendProofContract {
    pub(super) fn operation_ids(&self) -> Vec<&str> {
        match self {
            Self::AuthorizationMatrix { operation, .. }
            | Self::TransactionAtomicity { operation, .. }
            | Self::ConcurrentUpdate { operation, .. } => vec![operation],
            Self::ResourceRoundTrip {
                write_operation,
                read_operation,
                ..
            } => vec![write_operation, read_operation],
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum AuthorizationDecision {
    Allow,
    Deny,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuthorizationPrincipal {
    pub actor: String,
    pub tenant: String,
    pub decision: AuthorizationDecision,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuthorizationDenyPolicy {
    #[serde(default)]
    pub statuses: Vec<u16>,
    #[serde(default)]
    pub redacted_output_paths: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ControlledFailureWitness {
    pub input_path: String,
    pub value: Value,
    pub statuses: Vec<u16>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ConcurrencyPolicy {
    OptimisticVersion {
        resource: String,
        version_input_path: String,
        conflict_statuses: Vec<u16>,
    },
    Conserved {
        resource: String,
        delta_input_path: String,
        before_path: String,
        after_path: String,
    },
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum RoundTripCheck {
    Exact {
        write_input_path: String,
        read_output_path: String,
    },
    Utf8Sha256 {
        write_input_path: String,
        read_hash_output_path: String,
    },
    ByteSize {
        write_input_path: String,
        read_size_output_path: String,
    },
    MediaType {
        write_input_path: String,
        read_output_path: String,
    },
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourceLifecycleContract {
    pub name: String,
    #[serde(default)]
    pub consistency: ResourceConsistency,
    pub create: ResourceCreateContract,
    pub read: ResourceReadContract,
    #[serde(default)]
    pub update: Option<ResourceMutationContract>,
    #[serde(default)]
    pub delete: Option<ResourceMutationContract>,
    #[serde(default)]
    pub fields: Vec<ResourceFieldContract>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourceCreateContract {
    pub operation: String,
    pub output_identity_path: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourceReadContract {
    pub operation: String,
    pub input_identity_path: String,
    pub output_identity_path: String,
    #[serde(default)]
    pub absent_statuses: Vec<u16>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourceMutationContract {
    pub operation: String,
    pub input_identity_path: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourceFieldContract {
    pub read_output_path: String,
    #[serde(default)]
    pub create_output_path: Option<String>,
    #[serde(default)]
    pub update_input_path: Option<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum QueryComparison {
    Equal,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QueryFilterContract {
    pub input_path: String,
    pub item_path: String,
    pub comparison: QueryComparison,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum QuerySortDirection {
    Ascending,
    Descending,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum QuerySortType {
    String,
    Number,
    Boolean,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QuerySortContract {
    pub item_path: String,
    pub direction: QuerySortDirection,
    pub value_type: QuerySortType,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QueryPaginationContract {
    pub cursor_input_path: String,
    pub next_cursor_output_path: String,
    pub snapshot_input_path: String,
    #[serde(default)]
    pub reference_operation: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct FleetInvariant {
    #[serde(default)]
    pub same_build: bool,
    #[serde(default)]
    pub same_config_contract: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum BackendInvariant {
    Range {
        operation: String,
        path: String,
        #[serde(default)]
        min: Option<f64>,
        #[serde(default)]
        max: Option<f64>,
    },
    EqualsInput {
        operation: String,
        output_path: String,
        input_path: String,
    },
    Unique {
        operation: String,
        path: String,
    },
    Idempotent {
        operation: String,
    },
    QuerySemantics {
        operation: String,
        items_path: String,
        identity_path: String,
        consistency: ResourceConsistency,
        #[serde(default)]
        filters: Vec<QueryFilterContract>,
        #[serde(default)]
        sort: Option<QuerySortContract>,
        #[serde(default)]
        limit_input_path: Option<String>,
        #[serde(default)]
        pagination: Option<QueryPaginationContract>,
    },
    Conserved {
        operation: String,
        left_path: String,
        right_path: String,
    },
    Bounded {
        operation: String,
        value_path: String,
        limit_path: String,
    },
    Transition {
        operation: String,
        path: String,
        from: String,
        to: Vec<String>,
    },
}

impl<'de> Deserialize<'de> for BackendInvariant {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::Error;
        let value = Value::deserialize(deserializer)?;
        let object = value
            .as_object()
            .ok_or_else(|| D::Error::custom("an invariant must be a mapping"))?;
        let field = |name: &str| {
            object
                .get(name)
                .and_then(Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| D::Error::custom(format!("invariant needs {name}")))
        };
        if let Some(kind) = object.get("kind").and_then(Value::as_str) {
            let operation = || field("operation");
            return match kind {
                "range" => Ok(Self::Range {
                    operation: operation()?,
                    path: field("path")?,
                    min: object.get("min").and_then(Value::as_f64),
                    max: object.get("max").and_then(Value::as_f64),
                }),
                "equals-input" => Ok(Self::EqualsInput {
                    operation: operation()?,
                    output_path: field("outputPath")?,
                    input_path: field("inputPath")?,
                }),
                "unique" => Ok(Self::Unique {
                    operation: operation()?,
                    path: field("path")?,
                }),
                "idempotent" => Ok(Self::Idempotent {
                    operation: operation()?,
                }),
                "query-semantics" => {
                    const ALLOWED: &[&str] = &[
                        "kind",
                        "operation",
                        "itemsPath",
                        "identityPath",
                        "consistency",
                        "filters",
                        "sort",
                        "limitInputPath",
                        "pagination",
                    ];
                    if let Some(unknown) =
                        object.keys().find(|key| !ALLOWED.contains(&key.as_str()))
                    {
                        return Err(D::Error::custom(format!(
                            "unknown query-semantics field {unknown}"
                        )));
                    }
                    Ok(Self::QuerySemantics {
                        operation: operation()?,
                        items_path: field("itemsPath")?,
                        identity_path: field("identityPath")?,
                        consistency: object
                            .get("consistency")
                            .cloned()
                            .map(serde_json::from_value)
                            .transpose()
                            .map_err(D::Error::custom)?
                            .unwrap_or_default(),
                        filters: object
                            .get("filters")
                            .cloned()
                            .map(serde_json::from_value)
                            .transpose()
                            .map_err(D::Error::custom)?
                            .unwrap_or_default(),
                        sort: object
                            .get("sort")
                            .cloned()
                            .map(serde_json::from_value)
                            .transpose()
                            .map_err(D::Error::custom)?,
                        limit_input_path: object
                            .get("limitInputPath")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                        pagination: object
                            .get("pagination")
                            .cloned()
                            .map(serde_json::from_value)
                            .transpose()
                            .map_err(D::Error::custom)?,
                    })
                }
                "conserved" => Ok(Self::Conserved {
                    operation: operation()?,
                    left_path: field("leftPath")?,
                    right_path: field("rightPath")?,
                }),
                "bounded" => Ok(Self::Bounded {
                    operation: operation()?,
                    value_path: field("valuePath")?,
                    limit_path: field("limitPath")?,
                }),
                "transition" => Ok(Self::Transition {
                    operation: operation()?,
                    path: field("path")?,
                    from: field("from")?,
                    to: object
                        .get("to")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect(),
                }),
                _ => Err(D::Error::custom(format!("unknown invariant {kind}"))),
            };
        }
        let rules = object
            .iter()
            .filter_map(|(key, value)| value.as_str().map(|value| (key.clone(), value.into())))
            .collect::<BTreeMap<String, String>>();
        if rules.len() != 1 {
            return Err(D::Error::custom(
                "an invariant must contain exactly one rule",
            ));
        }
        let (kind, expression) = rules.into_iter().next().expect("checked length");
        let path = |value: &str| {
            format!(
                "$.{}",
                value
                    .trim()
                    .trim_start_matches("$.")
                    .trim_start_matches('.')
            )
        };
        let pair = |operator: &str| {
            expression
                .split_once(operator)
                .map(|(left, right)| (path(left), path(right)))
                .ok_or_else(|| D::Error::custom(format!("{kind} must contain {operator}")))
        };
        match kind.as_str() {
            "range" => {
                let (field, maximum) = expression
                    .split_once("<=")
                    .ok_or_else(|| D::Error::custom("range must contain <="))?;
                Ok(Self::Range {
                    operation: "*".into(),
                    path: path(field),
                    min: None,
                    max: Some(
                        maximum
                            .trim()
                            .parse()
                            .map_err(|_| D::Error::custom("range maximum must be numeric"))?,
                    ),
                })
            }
            "equals-input" => {
                let (output_path, input_path) = pair("==")?;
                Ok(Self::EqualsInput {
                    operation: "*".into(),
                    output_path,
                    input_path,
                })
            }
            "unique" => Ok(Self::Unique {
                operation: "*".into(),
                path: path(&expression),
            }),
            "idempotent" => Ok(Self::Idempotent {
                operation: expression.trim().into(),
            }),
            "conserved" => {
                let (left_path, right_path) = pair("==")?;
                Ok(Self::Conserved {
                    operation: "*".into(),
                    left_path,
                    right_path,
                })
            }
            "bounded" => {
                let (value_path, limit_path) = pair("<=")?;
                Ok(Self::Bounded {
                    operation: "*".into(),
                    value_path,
                    limit_path,
                })
            }
            "transition" => {
                let (from, targets) = expression
                    .split_once("->")
                    .ok_or_else(|| D::Error::custom("transition must contain ->"))?;
                Ok(Self::Transition {
                    operation: "*".into(),
                    path: "*".into(),
                    from: from.trim().into(),
                    to: targets
                        .split('|')
                        .map(|value| value.trim().into())
                        .collect(),
                })
            }
            _ => Err(D::Error::custom(format!("unknown invariant {kind}"))),
        }
    }
}
