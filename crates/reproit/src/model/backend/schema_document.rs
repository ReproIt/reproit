use anyhow::{bail, Context, Result};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// Load a JSON/YAML service schema and inline every local file or JSON-pointer
/// reference before import. Remote references stay explicit failures so a
/// supposedly hermetic scan never changes meaning with the network. Recursive
/// edges become an unconstrained schema at the cycle while their surrounding
/// object shape remains available to the contract importer.
pub fn load_service_document(path: &Path) -> Result<Value> {
    let path = path
        .canonicalize()
        .with_context(|| format!("resolving backend schema {}", path.display()))?;
    let document = read_schema_document(&path)?;
    resolve_schema_refs(&document, &document, &path, &mut BTreeSet::new(), 0)
}

fn read_schema_document(path: &Path) -> Result<Value> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("reading backend schema {}", path.display()))?;
    match path.extension().and_then(|value| value.to_str()) {
        Some("proto") => {
            let parent = path.parent().unwrap_or_else(|| Path::new("."));
            let descriptor = protox::compile([path], [parent])?;
            Ok(protobuf_descriptor_value(descriptor))
        }
        Some("graphql" | "gql") => graphql_sdl_document(&raw)
            .with_context(|| format!("parsing GraphQL SDL {}", path.display())),
        Some("yaml" | "yml") => serde_yaml::from_str(&raw)
            .with_context(|| format!("parsing backend schema {}", path.display())),
        _ => serde_json::from_str(&raw)
            .with_context(|| format!("parsing backend schema {}", path.display())),
    }
}

pub(super) fn graphql_sdl_document(raw: &str) -> Result<Value> {
    use graphql_parser::schema::{Definition, Type, TypeDefinition};
    let document = graphql_parser::schema::parse_schema::<String>(raw)
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    let mut kinds = BTreeMap::<String, &'static str>::new();
    for definition in &document.definitions {
        if let Definition::TypeDefinition(definition) = definition {
            let (name, kind) = match definition {
                TypeDefinition::Scalar(value) => (&value.name, "SCALAR"),
                TypeDefinition::Object(value) => (&value.name, "OBJECT"),
                TypeDefinition::Interface(value) => (&value.name, "INTERFACE"),
                TypeDefinition::Union(value) => (&value.name, "UNION"),
                TypeDefinition::Enum(value) => (&value.name, "ENUM"),
                TypeDefinition::InputObject(value) => (&value.name, "INPUT_OBJECT"),
            };
            kinds.insert(name.clone(), kind);
        }
    }
    let type_ref = |ty: &Type<'_, String>| graphql_sdl_type_ref(ty, &kinds);
    let mut types = Vec::new();
    let mut query = None;
    let mut mutation = None;
    let mut subscription = None;
    let mut implementations = BTreeMap::<String, Vec<String>>::new();
    for definition in &document.definitions {
        match definition {
            Definition::SchemaDefinition(schema) => {
                query = schema.query.clone();
                mutation = schema.mutation.clone();
                subscription = schema.subscription.clone();
            }
            Definition::TypeDefinition(TypeDefinition::Object(object)) => {
                for interface in &object.implements_interfaces {
                    implementations
                        .entry(interface.clone())
                        .or_default()
                        .push(object.name.clone());
                }
                let fields = object
                    .fields
                    .iter()
                    .map(|field| {
                        json!({
                            "name": field.name,
                            "args": field.arguments.iter().map(|argument| json!({
                                "name": argument.name,
                                "type": type_ref(&argument.value_type),
                            })).collect::<Vec<_>>(),
                            "type": type_ref(&field.field_type),
                        })
                    })
                    .collect::<Vec<_>>();
                types.push(json!({"kind":"OBJECT","name":object.name,"fields":fields}));
            }
            Definition::TypeDefinition(TypeDefinition::Interface(interface)) => {
                let fields = interface
                    .fields
                    .iter()
                    .map(|field| {
                        json!({
                            "name": field.name,
                            "args": field.arguments.iter().map(|argument| json!({
                                "name": argument.name,
                                "type": type_ref(&argument.value_type),
                            })).collect::<Vec<_>>(),
                            "type": type_ref(&field.field_type),
                        })
                    })
                    .collect::<Vec<_>>();
                types.push(json!({"kind":"INTERFACE","name":interface.name,"fields":fields}));
            }
            Definition::TypeDefinition(TypeDefinition::InputObject(object)) => {
                let fields = object
                    .fields
                    .iter()
                    .map(|field| json!({"name":field.name,"type":type_ref(&field.value_type)}))
                    .collect::<Vec<_>>();
                types.push(json!({"kind":"INPUT_OBJECT","name":object.name,"inputFields":fields}));
            }
            Definition::TypeDefinition(TypeDefinition::Enum(enumeration)) => {
                types.push(json!({
                    "kind":"ENUM",
                    "name":enumeration.name,
                    "enumValues": enumeration
                        .values
                        .iter()
                        .map(|value| json!({"name": value.name}))
                        .collect::<Vec<_>>()
                }));
            }
            Definition::TypeDefinition(TypeDefinition::Union(union)) => {
                types.push(json!({
                    "kind":"UNION",
                    "name":union.name,
                    "possibleTypes": union
                        .types
                        .iter()
                        .map(|name| json!({"name": name}))
                        .collect::<Vec<_>>()
                }));
            }
            Definition::TypeDefinition(TypeDefinition::Scalar(scalar)) => {
                types.push(json!({"kind":"SCALAR","name":scalar.name}));
            }
            _ => {}
        }
    }
    for value in &mut types {
        if value.get("kind").and_then(Value::as_str) == Some("INTERFACE") {
            if let Some(name) = value.get("name").and_then(Value::as_str) {
                value["possibleTypes"] = Value::Array(
                    implementations
                        .get(name)
                        .into_iter()
                        .flatten()
                        .map(|name| json!({"name":name}))
                        .collect(),
                );
            }
        }
    }
    let has_type = |name: &str| {
        types
            .iter()
            .any(|value| value.get("name").and_then(Value::as_str) == Some(name))
    };
    query = query.or_else(|| has_type("Query").then(|| "Query".into()));
    mutation = mutation.or_else(|| has_type("Mutation").then(|| "Mutation".into()));
    subscription = subscription.or_else(|| has_type("Subscription").then(|| "Subscription".into()));
    Ok(json!({"data":{"__schema":{
        "queryType": query.map(|name| json!({"name":name})),
        "mutationType": mutation.map(|name| json!({"name":name})),
        "subscriptionType": subscription.map(|name| json!({"name":name})),
        "types": types,
    }}}))
}

fn graphql_sdl_type_ref(
    ty: &graphql_parser::schema::Type<'_, String>,
    kinds: &BTreeMap<String, &'static str>,
) -> Value {
    use graphql_parser::schema::Type;
    match ty {
        Type::NamedType(name) => json!({
            "kind":kinds.get(name).copied().unwrap_or_else(|| graphql_named_kind(name)),
            "name":name,
            "ofType":null
        }),
        Type::ListType(inner) => {
            json!({"kind":"LIST","name":null,"ofType":graphql_sdl_type_ref(inner, kinds)})
        }
        Type::NonNullType(inner) => {
            json!({"kind":"NON_NULL","name":null,"ofType":graphql_sdl_type_ref(inner, kinds)})
        }
    }
}

fn graphql_named_kind(name: &str) -> &'static str {
    if matches!(name, "Int" | "Float" | "String" | "Boolean" | "ID") {
        "SCALAR"
    } else {
        "OBJECT"
    }
}

fn protobuf_descriptor_value(set: prost_types::FileDescriptorSet) -> Value {
    Value::Object(Map::from_iter([(
        "file".into(),
        Value::Array(
            set.file
                .into_iter()
                .map(|file| {
                    json!({
                        "package": file.package.unwrap_or_default(),
                        "messageType": file
                            .message_type
                            .into_iter()
                            .map(protobuf_message_value)
                            .collect::<Vec<_>>(),
                        "service": file.service.into_iter().map(|service| json!({
                            "name": service.name.unwrap_or_default(),
                            "method": service.method.into_iter().map(|method| json!({
                                "name": method.name.unwrap_or_default(),
                                "inputType": method.input_type.unwrap_or_default(),
                                "outputType": method.output_type.unwrap_or_default(),
                                "clientStreaming": method.client_streaming.unwrap_or(false),
                                "serverStreaming": method.server_streaming.unwrap_or(false),
                            })).collect::<Vec<_>>(),
                        })).collect::<Vec<_>>(),
                    })
                })
                .collect(),
        ),
    )]))
}

fn protobuf_message_value(message: prost_types::DescriptorProto) -> Value {
    json!({
        "name": message.name.unwrap_or_default(),
        "field": message.field.into_iter().map(|field| json!({
            "name": field.name.unwrap_or_default(),
            "number": field.number.unwrap_or_default(),
            "label": protobuf_label(field.label.unwrap_or_default()),
            "type": protobuf_type(field.r#type.unwrap_or_default()),
            "typeName": field.type_name.unwrap_or_default(),
        })).collect::<Vec<_>>(),
        "nestedType": message
            .nested_type
            .into_iter()
            .map(protobuf_message_value)
            .collect::<Vec<_>>(),
    })
}

fn protobuf_type(value: i32) -> &'static str {
    use prost_types::field_descriptor_proto::Type;
    match Type::try_from(value).ok() {
        Some(Type::Double) => "TYPE_DOUBLE",
        Some(Type::Float) => "TYPE_FLOAT",
        Some(Type::Int64) => "TYPE_INT64",
        Some(Type::Uint64) => "TYPE_UINT64",
        Some(Type::Int32) => "TYPE_INT32",
        Some(Type::Fixed64) => "TYPE_FIXED64",
        Some(Type::Fixed32) => "TYPE_FIXED32",
        Some(Type::Bool) => "TYPE_BOOL",
        Some(Type::String) => "TYPE_STRING",
        Some(Type::Group) => "TYPE_GROUP",
        Some(Type::Message) => "TYPE_MESSAGE",
        Some(Type::Bytes) => "TYPE_BYTES",
        Some(Type::Uint32) => "TYPE_UINT32",
        Some(Type::Enum) => "TYPE_ENUM",
        Some(Type::Sfixed32) => "TYPE_SFIXED32",
        Some(Type::Sfixed64) => "TYPE_SFIXED64",
        Some(Type::Sint32) => "TYPE_SINT32",
        Some(Type::Sint64) => "TYPE_SINT64",
        None => "TYPE_UNSPECIFIED",
    }
}

fn protobuf_label(value: i32) -> &'static str {
    use prost_types::field_descriptor_proto::Label;
    match Label::try_from(value).ok() {
        Some(Label::Optional) => "LABEL_OPTIONAL",
        Some(Label::Required) => "LABEL_REQUIRED",
        Some(Label::Repeated) => "LABEL_REPEATED",
        None => "LABEL_OPTIONAL",
    }
}

fn resolve_schema_refs(
    value: &Value,
    document: &Value,
    document_path: &Path,
    visiting: &mut BTreeSet<String>,
    depth: usize,
) -> Result<Value> {
    if depth > 128 {
        bail!(
            "backend schema reference depth exceeded 128 at {}",
            document_path.display()
        );
    }
    if let Some(reference) = value.get("$ref").and_then(Value::as_str) {
        if reference.starts_with("http://") || reference.starts_with("https://") {
            bail!(
                "remote backend schema reference {reference:?} is not hermetic; download and pin \
                 it locally"
            );
        }
        let (file, fragment) = reference.split_once('#').unwrap_or((reference, ""));
        let (target_path, target_document) = if file.is_empty() {
            (document_path.to_path_buf(), document.clone())
        } else {
            let target_path = document_path
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .join(file)
                .canonicalize()
                .with_context(|| {
                    format!(
                        "resolving backend schema reference {reference:?} from {}",
                        document_path.display()
                    )
                })?;
            let target_document = read_schema_document(&target_path)?;
            (target_path, target_document)
        };
        let pointer = if fragment.is_empty() {
            "".to_string()
        } else if fragment.starts_with('/') {
            fragment.to_string()
        } else {
            bail!("backend schema reference fragments must be JSON pointers: {reference}");
        };
        let identity = format!("{}#{pointer}", target_path.display());
        if !visiting.insert(identity.clone()) {
            return Ok(json!({}));
        }
        let target = if pointer.is_empty() {
            &target_document
        } else {
            target_document.pointer(&pointer).with_context(|| {
                format!(
                    "backend schema reference {reference:?} points outside {}",
                    target_path.display()
                )
            })?
        };
        let mut resolved =
            resolve_schema_refs(target, &target_document, &target_path, visiting, depth + 1)?;
        visiting.remove(&identity);
        if let (Some(resolved), Some(siblings)) = (resolved.as_object_mut(), value.as_object()) {
            for (name, sibling) in siblings {
                if name != "$ref" {
                    resolved.insert(
                        name.clone(),
                        resolve_schema_refs(sibling, document, document_path, visiting, depth + 1)?,
                    );
                }
            }
        }
        return Ok(resolved);
    }
    match value {
        Value::Object(object) => Ok(Value::Object(
            object
                .iter()
                .map(|(name, value)| {
                    Ok((
                        name.clone(),
                        resolve_schema_refs(value, document, document_path, visiting, depth + 1)?,
                    ))
                })
                .collect::<Result<Map<String, Value>>>()?,
        )),
        Value::Array(values) => Ok(Value::Array(
            values
                .iter()
                .map(|value| {
                    resolve_schema_refs(value, document, document_path, visiting, depth + 1)
                })
                .collect::<Result<Vec<_>>>()?,
        )),
        _ => Ok(value.clone()),
    }
}
