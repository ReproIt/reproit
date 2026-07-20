use super::*;

pub(super) fn load_document(path: &Path) -> Result<Value> {
    backend::load_service_document(path)
}

pub(super) fn service_base_url(document: &Value) -> Result<String> {
    if let Ok(override_url) = std::env::var("REPROIT_BACKEND_URL") {
        validate_base_url(&override_url)?;
        return Ok(override_url.trim_end_matches('/').to_string());
    }
    let server = document
        .get("servers")
        .and_then(Value::as_array)
        .and_then(|servers| servers.first())
        .and_then(|server| server.get("url").and_then(Value::as_str))
        .context(
            "the schema has no absolute server URL; set REPROIT_BACKEND_URL to the disposable \
             service",
        )?;
    let mut resolved = server.to_string();
    if let Some(variables) = document
        .pointer("/servers/0/variables")
        .and_then(Value::as_object)
    {
        for (name, variable) in variables {
            if let Some(default) = variable.get("default").and_then(value_as_text) {
                resolved = resolved.replace(&format!("{{{name}}}"), &default);
            }
        }
    }
    validate_base_url(&resolved)?;
    Ok(resolved.trim_end_matches('/').to_string())
}

pub(super) fn validate_base_url(value: &str) -> Result<()> {
    let url = value
        .parse::<reqwest::Url>()
        .with_context(|| format!("invalid backend service URL {value:?}"))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        bail!("backend service URL must be absolute HTTP or HTTPS: {value}");
    }
    Ok(())
}

pub(super) fn openapi_endpoints(document: &Value) -> Vec<Endpoint> {
    let contracts = backend::import_openapi(document)
        .into_iter()
        .map(|contract| (contract.id.clone(), contract))
        .collect::<BTreeMap<_, _>>();
    let mut endpoints = Vec::new();
    let Some(paths) = document.get("paths").and_then(Value::as_object) else {
        return endpoints;
    };
    for (path, item) in paths {
        let Some(methods) = item.as_object() else {
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
            let Some(contract) = contracts.get(&id).cloned() else {
                continue;
            };
            let request_content = operation
                .pointer("/requestBody/content")
                .and_then(Value::as_object);
            let content_type = request_content.and_then(preferred_request_content_type);
            let has_parameters = item
                .get("parameters")
                .and_then(Value::as_array)
                .is_some_and(|values| !values.is_empty())
                || operation
                    .get("parameters")
                    .and_then(Value::as_array)
                    .is_some_and(|values| !values.is_empty());
            endpoints.push(Endpoint {
                contract,
                method: method.to_ascii_uppercase(),
                path: path.clone(),
                body_only: request_content.is_some() && !has_parameters,
                content_type,
                response_field: None,
                policy: BackendPolicy::default(),
                transport: Transport::Http,
                schema_source: None,
                client_streaming: false,
                server_streaming: false,
            });
        }
    }
    endpoints.sort_by(|left, right| left.contract.id.cmp(&right.contract.id));
    endpoints
}

pub(super) fn graphql_endpoints(document: &Value) -> Vec<Endpoint> {
    let Some(schema) = document
        .pointer("/data/__schema")
        .or_else(|| document.get("__schema"))
    else {
        return Vec::new();
    };
    let types = schema
        .get("types")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|value| Some((value.get("name")?.as_str()?, value)))
        .collect::<BTreeMap<_, _>>();
    let contracts = backend::import_service_schema(document)
        .into_iter()
        .map(|contract| (contract.id.clone(), contract))
        .collect::<BTreeMap<_, _>>();
    let mut endpoints = Vec::new();
    for (root_key, keyword) in [("queryType", "query"), ("mutationType", "mutation")] {
        let Some(root_name) = schema
            .pointer(&format!("/{root_key}/name"))
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
            let Some(name) = field.get("name").and_then(Value::as_str) else {
                continue;
            };
            let Some(contract) = contracts.get(name).cloned() else {
                continue;
            };
            let arguments = field
                .get("args")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|argument| {
                    Some((
                        argument.get("name")?.as_str()?,
                        graphql_type_name(argument.get("type")?)?,
                    ))
                })
                .collect::<Vec<_>>();
            let declarations = arguments
                .iter()
                .map(|(name, ty)| format!("${name}: {ty}"))
                .collect::<Vec<_>>()
                .join(", ");
            let calls = arguments
                .iter()
                .map(|(name, _)| format!("{name}: ${name}"))
                .collect::<Vec<_>>()
                .join(", ");
            let selection = contract
                .output
                .as_ref()
                .map(|domain| graphql_selection(domain, 0))
                .unwrap_or_default();
            let declaration = if declarations.is_empty() {
                String::new()
            } else {
                format!("({declarations})")
            };
            let call = if calls.is_empty() {
                String::new()
            } else {
                format!("({calls})")
            };
            let selection = if selection.is_empty() {
                String::new()
            } else {
                format!(" {{ {selection} }}")
            };
            let query = format!("{keyword} Reproit{declaration} {{ {name}{call}{selection} }}");
            endpoints.push(Endpoint {
                contract,
                method: "POST".into(),
                path: String::new(),
                body_only: true,
                content_type: Some("application/json".into()),
                response_field: Some(name.into()),
                policy: BackendPolicy::default(),
                transport: Transport::Http,
                schema_source: None,
                client_streaming: false,
                server_streaming: false,
            });
            if let Some(endpoint) = endpoints.last_mut() {
                endpoint.path = query;
            }
        }
    }
    endpoints
}

pub(super) fn graphql_type_name(reference: &Value) -> Option<String> {
    match reference.get("kind")?.as_str()? {
        "NON_NULL" => Some(format!("{}!", graphql_type_name(reference.get("ofType")?)?)),
        "LIST" => Some(format!(
            "[{}]",
            graphql_type_name(reference.get("ofType")?)?
        )),
        _ => reference.get("name")?.as_str().map(str::to_string),
    }
}

pub(super) fn graphql_selection(domain: &ValueDomain, depth: usize) -> String {
    if depth > MAX_GRAPHQL_SELECTION_DEPTH {
        return "__typename".into();
    }
    match domain {
        ValueDomain::Object { properties, .. } => properties
            .iter()
            .map(|(name, domain)| {
                let nested = graphql_selection(domain, depth + 1);
                if nested.is_empty() {
                    name.clone()
                } else {
                    format!("{name} {{ {nested} }}")
                }
            })
            .collect::<Vec<_>>()
            .join(" "),
        ValueDomain::Array { items, .. } => graphql_selection(items, depth + 1),
        ValueDomain::OneOf { variants } => variants
            .iter()
            .map(|variant| graphql_selection(variant, depth + 1))
            .find(|selection| !selection.is_empty())
            .unwrap_or_default(),
        ValueDomain::AllOf { variants } => variants
            .iter()
            .map(|variant| graphql_selection(variant, depth + 1))
            .find(|selection| !selection.is_empty())
            .unwrap_or_default(),
        ValueDomain::GraphqlAbstract { variants } => {
            let fragments = variants
                .iter()
                .map(|(name, domain)| {
                    format!(
                        "... on {name} {{ {} }}",
                        graphql_selection(domain, depth + 1)
                    )
                })
                .collect::<Vec<_>>()
                .join(" ");
            format!("__typename {fragments}")
        }
        _ => String::new(),
    }
}

pub(super) fn grpc_endpoints(document: &Value) -> Vec<Endpoint> {
    let streaming = grpc_streaming_modes(document);
    backend::import_service_schema(document)
        .into_iter()
        .map(|mut contract| {
            let (client_streaming, server_streaming) =
                streaming.get(&contract.id).copied().unwrap_or_default();
            if client_streaming {
                contract.input = contract.input.take().map(|input| ValueDomain::Array {
                    items: Box::new(input),
                    min_items: Some(1),
                    max_items: Some(3),
                    unique: false,
                });
            }
            if server_streaming {
                contract.output = contract.output.take().map(|output| ValueDomain::Array {
                    items: Box::new(output),
                    min_items: None,
                    max_items: None,
                    unique: false,
                });
            }
            Endpoint {
                path: contract.id.clone(),
                contract,
                method: "GRPC".into(),
                body_only: true,
                content_type: Some("application/json".into()),
                response_field: None,
                policy: BackendPolicy::default(),
                transport: Transport::Grpc,
                schema_source: None,
                client_streaming,
                server_streaming,
            }
        })
        .collect()
}

pub(super) fn grpc_streaming_modes(document: &Value) -> BTreeMap<String, (bool, bool)> {
    let mut modes = BTreeMap::new();
    for file in document
        .get("file")
        .or_else(|| document.get("files"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let package = file.get("package").and_then(Value::as_str).unwrap_or("");
        for service in file
            .get("service")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(service_name) = service.get("name").and_then(Value::as_str) else {
                continue;
            };
            let prefix = if package.is_empty() {
                service_name.to_string()
            } else {
                format!("{package}.{service_name}")
            };
            for method in service
                .get("method")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let Some(name) = method.get("name").and_then(Value::as_str) else {
                    continue;
                };
                modes.insert(
                    format!("{prefix}/{name}"),
                    (
                        method
                            .get("clientStreaming")
                            .or_else(|| method.get("client_streaming"))
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                        method
                            .get("serverStreaming")
                            .or_else(|| method.get("server_streaming"))
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                    ),
                );
            }
        }
    }
    modes
}

pub(super) fn preferred_request_content_type(content: &Map<String, Value>) -> Option<String> {
    content
        .keys()
        .find(|media| {
            let normalized = media.to_ascii_lowercase();
            normalized == "application/json" || normalized.ends_with("+json")
        })
        .cloned()
        .or_else(|| {
            content
                .contains_key("application/x-www-form-urlencoded")
                .then(|| "application/x-www-form-urlencoded".to_string())
        })
}
