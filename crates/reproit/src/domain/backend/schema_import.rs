use super::{Authority, IdempotencyResponseReplay, OperationContract, ValueDomain};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

pub fn import_service_schema(document: &Value) -> Vec<OperationContract> {
    if document.get("openapi").is_some() || document.get("swagger").is_some() {
        import_openapi(document)
    } else if document.pointer("/data/__schema").is_some() || document.get("__schema").is_some() {
        import_graphql(document)
    } else if document.get("file").is_some() || document.get("files").is_some() {
        import_protobuf_descriptor(document)
    } else {
        Vec::new()
    }
}

pub fn import_openapi(document: &Value) -> Vec<OperationContract> {
    let Some(paths) = document.get("paths").and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut operations = Vec::new();
    for (path, path_item) in paths {
        let Some(methods) = path_item.as_object() else {
            continue;
        };
        for (method, operation) in methods {
            if !["get", "post", "put", "patch", "delete", "head", "options"]
                .contains(&method.as_str())
            {
                continue;
            }
            let id = operation
                .get("operationId")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| format!("{} {}", method.to_ascii_uppercase(), path));
            let body = operation
                .pointer("/requestBody/content")
                .and_then(Value::as_object)
                .and_then(|content| safe_content_domain(content, document, false));
            let input = openapi_input(path_item, operation, body, document);
            let mut success_statuses = Vec::new();
            let mut outputs_by_status = BTreeMap::new();
            if let Some(responses) = operation.get("responses").and_then(Value::as_object) {
                for (status, response) in responses {
                    let Some(code) = status.parse::<u16>().ok() else {
                        continue;
                    };
                    if (200..400).contains(&code) {
                        success_statuses.push(code);
                        if let Some(domain) = response
                            .get("content")
                            .and_then(Value::as_object)
                            .and_then(|content| safe_content_domain(content, document, true))
                        {
                            outputs_by_status.insert(code, domain);
                        }
                    }
                }
            }
            let output = match outputs_by_status.len() {
                0 => None,
                1 => outputs_by_status.values().next().cloned(),
                _ => Some(ValueDomain::OneOf {
                    variants: outputs_by_status.values().cloned().collect(),
                }),
            };
            operations.push(OperationContract {
                id,
                authority: Authority::Schema,
                input,
                output,
                outputs_by_status,
                success_statuses,
                read_only: matches!(method.as_str(), "get" | "head" | "options"),
                idempotent: matches!(
                    method.as_str(),
                    "get" | "put" | "delete" | "head" | "options"
                ),
                idempotency_response_replay: IdempotencyResponseReplay::Unspecified,
                tenant_isolated: false,
                promised_effects: Vec::new(),
            });
        }
    }
    operations
}

/// Import only encodings whose decoded value is structurally unambiguous.
/// JSON (including vendor `+json`) carries the complete JSON domain. Plain text
/// is safe only for a string schema, and form-urlencoded is safe only for an
/// object schema. XML, multipart, and binary bodies remain guidance-free until
/// an adapter can prove their decoded structure.
fn safe_content_domain(
    content: &serde_json::Map<String, Value>,
    document: &Value,
    response: bool,
) -> Option<ValueDomain> {
    let mut domains = Vec::new();
    for (media_type, media) in content {
        let media_type = media_type
            .split(';')
            .next()
            .unwrap_or(media_type)
            .trim()
            .to_ascii_lowercase();
        let Some(domain) = media
            .get("schema")
            .and_then(|schema| schema_domain(schema, document))
        else {
            continue;
        };
        let safe = media_type == "application/json"
            || media_type.ends_with("+json")
            || (media_type == "text/plain" && domain_is_string(&domain))
            || (!response
                && media_type == "application/x-www-form-urlencoded"
                && domain_is_object(&domain));
        if safe {
            domains.push(domain);
        }
    }
    match domains.len() {
        0 => None,
        1 => domains.pop(),
        _ => Some(ValueDomain::OneOf { variants: domains }),
    }
}

fn domain_is_string(domain: &ValueDomain) -> bool {
    match domain {
        ValueDomain::String { .. } => true,
        ValueDomain::OneOf { variants } => {
            variants.iter().any(domain_is_string)
                && variants.iter().all(|variant| {
                    matches!(variant, ValueDomain::Null) || domain_is_string(variant)
                })
        }
        ValueDomain::AllOf { variants } => {
            !variants.is_empty() && variants.iter().all(domain_is_string)
        }
        _ => false,
    }
}

fn domain_is_object(domain: &ValueDomain) -> bool {
    match domain {
        ValueDomain::Object { .. } => true,
        ValueDomain::OneOf { variants } => {
            variants.iter().any(domain_is_object)
                && variants.iter().all(|variant| {
                    matches!(variant, ValueDomain::Null) || domain_is_object(variant)
                })
        }
        ValueDomain::AllOf { variants } => {
            !variants.is_empty() && variants.iter().all(domain_is_object)
        }
        _ => false,
    }
}

fn openapi_input(
    path_item: &Value,
    operation: &Value,
    body: Option<ValueDomain>,
    document: &Value,
) -> Option<ValueDomain> {
    let mut groups = BTreeMap::<String, ValueDomain>::new();
    let mut required_groups = BTreeSet::new();
    let parameters = path_item
        .get("parameters")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .chain(
            operation
                .get("parameters")
                .and_then(Value::as_array)
                .into_iter()
                .flatten(),
        );
    let mut fields = BTreeMap::<String, (BTreeMap<String, ValueDomain>, BTreeSet<String>)>::new();
    for raw in parameters {
        let parameter = resolve_local_ref(raw, document).unwrap_or(raw);
        let Some(location) = parameter.get("in").and_then(Value::as_str) else {
            continue;
        };
        // Cookie values are secrets by default. Object/deepObject serialization
        // is not canonical across clients, so neither can be exact evidence.
        if !matches!(location, "path" | "query" | "header")
            || parameter.get("content").is_some()
            || parameter.get("style").and_then(Value::as_str) == Some("deepObject")
        {
            continue;
        }
        let Some(name) = parameter.get("name").and_then(Value::as_str) else {
            continue;
        };
        let Some(domain) = parameter
            .get("schema")
            .and_then(|schema| schema_domain(schema, document))
        else {
            continue;
        };
        if domain_is_object(&domain) {
            continue;
        }
        let group = match location {
            "path" => "path",
            "query" => "query",
            _ => "headers",
        };
        let normalized = if location == "header" {
            name.to_ascii_lowercase()
        } else {
            name.to_string()
        };
        let entry = fields.entry(group.into()).or_default();
        entry.0.insert(normalized.clone(), domain);
        if location == "path" || parameter.get("required").and_then(Value::as_bool) == Some(true) {
            entry.1.insert(normalized);
            required_groups.insert(group.into());
        }
    }
    for (group, (properties, required)) in fields {
        groups.insert(
            group,
            ValueDomain::Object {
                required,
                properties,
                additional: true,
            },
        );
    }
    if groups.is_empty() {
        return body;
    }
    if let Some(body) = body {
        groups.insert("body".into(), body);
        if operation
            .pointer("/requestBody/required")
            .and_then(Value::as_bool)
            == Some(true)
        {
            required_groups.insert("body".into());
        }
    }
    Some(ValueDomain::Object {
        required: required_groups,
        properties: groups,
        additional: false,
    })
}

fn resolve_local_ref<'a>(value: &'a Value, document: &'a Value) -> Option<&'a Value> {
    let reference = value.get("$ref")?.as_str()?.strip_prefix('#')?;
    document.pointer(reference)
}

pub(super) fn import_graphql(document: &Value) -> Vec<OperationContract> {
    let schema = document
        .pointer("/data/__schema")
        .or_else(|| document.get("__schema"));
    let Some(schema) = schema else {
        return Vec::new();
    };
    let types = schema
        .get("types")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|value| {
            value
                .get("name")
                .and_then(Value::as_str)
                .map(|name| (name, value))
        })
        .collect::<BTreeMap<_, _>>();
    let roots = [
        ("queryType", true),
        ("mutationType", false),
        ("subscriptionType", true),
    ];
    let mut operations = Vec::new();
    for (root_key, read_only) in roots {
        let Some(root_name) = schema
            .get(root_key)
            .and_then(|value| value.get("name"))
            .and_then(Value::as_str)
        else {
            continue;
        };
        let Some(root) = types.get(root_name) else {
            continue;
        };
        for field in root
            .get("fields")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(id) = field.get("name").and_then(Value::as_str) else {
                continue;
            };
            let args = field
                .get("args")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|argument| {
                    let name = argument.get("name")?.as_str()?.to_string();
                    let domain = graphql_domain(
                        argument.get("type")?,
                        &types,
                        &mut BTreeSet::new(),
                        GraphqlDomainContext::Input,
                    )?;
                    Some((name, domain, graphql_non_null(argument.get("type")?)))
                })
                .collect::<Vec<_>>();
            let input = (!args.is_empty()).then(|| ValueDomain::Object {
                required: args
                    .iter()
                    .filter(|(_, _, required)| *required)
                    .map(|(name, _, _)| name.clone())
                    .collect(),
                properties: args
                    .into_iter()
                    .map(|(name, domain, _)| (name, domain))
                    .collect(),
                additional: false,
            });
            operations.push(OperationContract {
                id: id.to_string(),
                authority: Authority::Schema,
                input,
                output: field.get("type").and_then(|value| {
                    graphql_domain(
                        value,
                        &types,
                        &mut BTreeSet::new(),
                        GraphqlDomainContext::Output,
                    )
                }),
                outputs_by_status: BTreeMap::new(),
                success_statuses: Vec::new(),
                read_only,
                idempotent: read_only,
                idempotency_response_replay: IdempotencyResponseReplay::Unspecified,
                tenant_isolated: false,
                promised_effects: Vec::new(),
            });
        }
    }
    operations
}

fn graphql_non_null(reference: &Value) -> bool {
    reference.get("kind").and_then(Value::as_str) == Some("NON_NULL")
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum GraphqlDomainContext {
    Input,
    Output,
}

fn graphql_domain(
    reference: &Value,
    types: &BTreeMap<&str, &Value>,
    visiting: &mut BTreeSet<String>,
    context: GraphqlDomainContext,
) -> Option<ValueDomain> {
    let kind = reference.get("kind").and_then(Value::as_str)?;
    if kind == "NON_NULL" {
        return graphql_non_null_domain(reference.get("ofType")?, types, visiting, context);
    }
    let domain = graphql_non_null_domain(reference, types, visiting, context)?;
    Some(ValueDomain::OneOf {
        variants: vec![ValueDomain::Null, domain],
    })
}

fn graphql_non_null_domain(
    reference: &Value,
    types: &BTreeMap<&str, &Value>,
    visiting: &mut BTreeSet<String>,
    context: GraphqlDomainContext,
) -> Option<ValueDomain> {
    let kind = reference.get("kind").and_then(Value::as_str)?;
    if kind == "LIST" {
        return Some(ValueDomain::Array {
            items: Box::new(graphql_domain(
                reference.get("ofType")?,
                types,
                visiting,
                context,
            )?),
            min_items: None,
            max_items: None,
            unique: false,
        });
    }
    let name = reference
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("String");
    match kind {
        "SCALAR" => Some(match name {
            "Int" => ValueDomain::Integer {
                min: None,
                max: None,
            },
            "Float" => ValueDomain::Number,
            "Boolean" => ValueDomain::Boolean,
            "ID" => ValueDomain::Resource {
                resource: "graphql-id".into(),
            },
            _ => ValueDomain::String {
                min_length: None,
                max_length: None,
                pattern: None,
                format: None,
                variants: Vec::new(),
            },
        }),
        "ENUM" => Some(ValueDomain::String {
            min_length: None,
            max_length: None,
            pattern: None,
            format: None,
            variants: types
                .get(name)
                .and_then(|value| value.get("enumValues"))
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|value| value.get("name").and_then(Value::as_str))
                .map(str::to_string)
                .collect(),
        }),
        "INTERFACE" | "UNION" if context == GraphqlDomainContext::Output => {
            let definition = types.get(name)?;
            let variants = definition
                .get("possibleTypes")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|possible| {
                    let possible_name = possible.get("name")?.as_str()?.to_string();
                    let reference = json!({"kind":"OBJECT","name":possible_name});
                    let domain = graphql_non_null_domain(&reference, types, visiting, context)?;
                    Some((possible_name, domain))
                })
                .collect::<BTreeMap<_, _>>();
            Some(ValueDomain::GraphqlAbstract { variants })
        }
        "OBJECT" | "INPUT_OBJECT" => {
            if !visiting.insert(name.to_string()) {
                return Some(ValueDomain::Any);
            }
            let definition = types.get(name)?;
            let fields = definition
                .get(if kind == "OBJECT" {
                    "fields"
                } else {
                    "inputFields"
                })
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .collect::<Vec<_>>();
            let required = fields
                .iter()
                .filter(|field| field.get("type").is_some_and(graphql_non_null))
                .filter_map(|field| field.get("name").and_then(Value::as_str))
                .map(str::to_string)
                .collect();
            let properties = fields
                .into_iter()
                .filter_map(|field| {
                    let field_name = field.get("name")?.as_str()?.to_string();
                    let domain = graphql_domain(field.get("type")?, types, visiting, context)?;
                    Some((field_name, domain))
                })
                .collect();
            visiting.remove(name);
            Some(ValueDomain::Object {
                // A GraphQL response contains only the client's selection set.
                // Introspection describes the complete object type, not the
                // fields selected by this invocation, so requiring every
                // NON_NULL schema field would reject valid partial responses.
                // Keep validating selected fields, but leave presence open
                // until runtime evidence carries a normalized selection set.
                required: if context == GraphqlDomainContext::Input {
                    required
                } else {
                    BTreeSet::new()
                },
                properties,
                // `__typename` is always selectable but is not part of the
                // ordinary field list returned by introspection.
                additional: context == GraphqlDomainContext::Output,
            })
        }
        _ => Some(ValueDomain::Any),
    }
}

fn import_protobuf_descriptor(document: &Value) -> Vec<OperationContract> {
    let files = document
        .get("file")
        .or_else(|| document.get("files"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    let mut messages = BTreeMap::<String, &Value>::new();
    for file in &files {
        let package = file.get("package").and_then(Value::as_str).unwrap_or("");
        for message in file
            .get("messageType")
            .or_else(|| file.get("message_type"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            collect_protobuf_messages(package, "", message, &mut messages);
        }
    }
    let mut operations = Vec::new();
    for file in files {
        let package = file.get("package").and_then(Value::as_str).unwrap_or("");
        for service in file
            .get("service")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let service_name = service
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("Service");
            for method in service
                .get("method")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let method_name = method
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("Method");
                let prefix = if package.is_empty() {
                    service_name.to_string()
                } else {
                    format!("{package}.{service_name}")
                };
                let input = method
                    .get("inputType")
                    .or_else(|| method.get("input_type"))
                    .and_then(Value::as_str)
                    .and_then(|name| {
                        protobuf_message_domain(name, &messages, &mut BTreeSet::new())
                    });
                let output = method
                    .get("outputType")
                    .or_else(|| method.get("output_type"))
                    .and_then(Value::as_str)
                    .and_then(|name| {
                        protobuf_message_domain(name, &messages, &mut BTreeSet::new())
                    });
                operations.push(OperationContract {
                    id: format!("{prefix}/{method_name}"),
                    authority: Authority::Schema,
                    input,
                    output,
                    outputs_by_status: BTreeMap::new(),
                    success_statuses: Vec::new(),
                    read_only: false,
                    idempotent: false,
                    idempotency_response_replay: IdempotencyResponseReplay::Unspecified,
                    tenant_isolated: false,
                    promised_effects: Vec::new(),
                });
            }
        }
    }
    operations
}

fn collect_protobuf_messages<'a>(
    package: &str,
    parent: &str,
    message: &'a Value,
    messages: &mut BTreeMap<String, &'a Value>,
) {
    let Some(name) = message.get("name").and_then(Value::as_str) else {
        return;
    };
    let local = if parent.is_empty() {
        name.to_string()
    } else {
        format!("{parent}.{name}")
    };
    let qualified = if package.is_empty() {
        format!(".{local}")
    } else {
        format!(".{package}.{local}")
    };
    messages.insert(qualified, message);
    for nested in message
        .get("nestedType")
        .or_else(|| message.get("nested_type"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        collect_protobuf_messages(package, &local, nested, messages);
    }
}

fn protobuf_message_domain(
    name: &str,
    messages: &BTreeMap<String, &Value>,
    visiting: &mut BTreeSet<String>,
) -> Option<ValueDomain> {
    if !visiting.insert(name.to_string()) {
        return Some(ValueDomain::Any);
    }
    let message = messages.get(name)?;
    let mut properties = BTreeMap::new();
    for field in message
        .get("field")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(field_name) = field.get("name").and_then(Value::as_str) else {
            continue;
        };
        let kind = field
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("TYPE_STRING");
        let mut domain = match kind {
            "TYPE_BOOL" => ValueDomain::Boolean,
            "TYPE_DOUBLE" | "TYPE_FLOAT" => ValueDomain::Number,
            "TYPE_INT32" | "TYPE_UINT32" | "TYPE_SINT32" | "TYPE_FIXED32" | "TYPE_SFIXED32" => {
                ValueDomain::Integer {
                    min: None,
                    max: None,
                }
            }
            "TYPE_INT64" | "TYPE_SINT64" | "TYPE_SFIXED64" => {
                ValueDomain::ProtoInteger64 { signed: true }
            }
            "TYPE_UINT64" | "TYPE_FIXED64" => ValueDomain::ProtoInteger64 { signed: false },
            "TYPE_MESSAGE" => field
                .get("typeName")
                .or_else(|| field.get("type_name"))
                .and_then(Value::as_str)
                .and_then(|nested| protobuf_message_domain(nested, messages, visiting))
                .unwrap_or(ValueDomain::Any),
            "TYPE_ENUM" | "TYPE_STRING" | "TYPE_BYTES" => ValueDomain::String {
                min_length: None,
                max_length: None,
                pattern: None,
                format: None,
                variants: Vec::new(),
            },
            _ => ValueDomain::Any,
        };
        if field.get("label").and_then(Value::as_str) == Some("LABEL_REPEATED") {
            domain = ValueDomain::Array {
                items: Box::new(domain),
                min_items: None,
                max_items: None,
                unique: false,
            };
        }
        properties.insert(field_name.to_string(), domain);
    }
    visiting.remove(name);
    Some(ValueDomain::Object {
        required: BTreeSet::new(),
        properties,
        additional: false,
    })
}

fn schema_domain(schema: &Value, document: &Value) -> Option<ValueDomain> {
    schema_domain_inner(schema, document, &mut BTreeSet::new())
}

fn schema_domain_inner(
    schema: &Value,
    document: &Value,
    visiting_refs: &mut BTreeSet<String>,
) -> Option<ValueDomain> {
    let nullable = document
        .get("openapi")
        .and_then(Value::as_str)
        .is_some_and(|version| version.starts_with("3.0."))
        && schema.get("nullable").and_then(Value::as_bool) == Some(true);
    let domain = schema_domain_non_null(schema, document, visiting_refs)?;
    Some(if nullable {
        nullable_domain(domain)
    } else {
        domain
    })
}

fn schema_domain_non_null(
    schema: &Value,
    document: &Value,
    visiting_refs: &mut BTreeSet<String>,
) -> Option<ValueDomain> {
    if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
        let pointer = reference.strip_prefix('#')?;
        if !visiting_refs.insert(pointer.to_string()) {
            return Some(ValueDomain::Any);
        }
        let domain = document
            .pointer(pointer)
            .and_then(|schema| schema_domain_inner(schema, document, visiting_refs));
        visiting_refs.remove(pointer);
        return domain;
    }
    if let Some(one_of) = schema
        .get("oneOf")
        .or_else(|| schema.get("anyOf"))
        .and_then(Value::as_array)
    {
        return Some(ValueDomain::OneOf {
            variants: one_of
                .iter()
                .filter_map(|value| schema_domain_inner(value, document, visiting_refs))
                .collect(),
        });
    }
    if let Some(all_of) = schema.get("allOf").and_then(Value::as_array) {
        return Some(ValueDomain::AllOf {
            variants: all_of
                .iter()
                .filter_map(|value| schema_domain_inner(value, document, visiting_refs))
                .collect(),
        });
    }
    if let Some(value) = schema.get("const") {
        return Some(ValueDomain::Literal {
            value: value.clone(),
        });
    }
    match schema.get("type").and_then(Value::as_str) {
        Some("null") => Some(ValueDomain::Null),
        Some("boolean") => Some(ValueDomain::Boolean),
        Some("integer") => Some(ValueDomain::Integer {
            min: schema.get("minimum").and_then(Value::as_i64),
            max: schema.get("maximum").and_then(Value::as_i64),
        }),
        Some("number") => Some(ValueDomain::Number),
        Some("string") => Some(ValueDomain::String {
            min_length: schema
                .get("minLength")
                .and_then(Value::as_u64)
                .map(|v| v as usize),
            max_length: schema
                .get("maxLength")
                .and_then(Value::as_u64)
                .map(|v| v as usize),
            pattern: schema
                .get("pattern")
                .and_then(Value::as_str)
                .map(str::to_string),
            format: schema
                .get("format")
                .and_then(Value::as_str)
                .map(str::to_string),
            variants: schema
                .get("enum")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect(),
        }),
        Some("array") => Some(ValueDomain::Array {
            items: Box::new(
                schema
                    .get("items")
                    .and_then(|value| schema_domain_inner(value, document, visiting_refs))
                    .unwrap_or(ValueDomain::Any),
            ),
            min_items: schema
                .get("minItems")
                .and_then(Value::as_u64)
                .map(|v| v as usize),
            max_items: schema
                .get("maxItems")
                .and_then(Value::as_u64)
                .map(|v| v as usize),
            unique: schema
                .get("uniqueItems")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        }),
        Some("object") | None if schema.get("properties").is_some() => Some(ValueDomain::Object {
            required: schema
                .get("required")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect(),
            properties: schema
                .get("properties")
                .and_then(Value::as_object)
                .into_iter()
                .flatten()
                .filter_map(|(name, value)| {
                    schema_domain_inner(value, document, visiting_refs)
                        .map(|domain| (name.clone(), domain))
                })
                .collect(),
            additional: schema
                .get("additionalProperties")
                .and_then(Value::as_bool)
                .unwrap_or(true),
        }),
        _ => Some(ValueDomain::Any),
    }
}

fn nullable_domain(domain: ValueDomain) -> ValueDomain {
    match domain {
        ValueDomain::Null => ValueDomain::Null,
        ValueDomain::OneOf { mut variants } => {
            if !variants
                .iter()
                .any(|variant| matches!(variant, ValueDomain::Null))
            {
                variants.insert(0, ValueDomain::Null);
            }
            ValueDomain::OneOf { variants }
        }
        domain => ValueDomain::OneOf {
            variants: vec![ValueDomain::Null, domain],
        },
    }
}
