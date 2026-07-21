use super::*;

pub(super) fn normalized_selection_path(path: &str) -> Option<Vec<(String, bool)>> {
    static NAME: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let name = NAME.get_or_init(|| {
        regex::Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*$").expect("the GraphQL name regex is valid")
    });
    let mut out = Vec::new();
    for raw in path.split('.') {
        let (raw, array) = raw
            .strip_suffix("[]")
            .map_or((raw, false), |field| (field, true));
        if !name.is_match(raw) {
            return None;
        }
        out.push((raw.to_string(), array));
    }
    (!out.is_empty()).then_some(out)
}

pub(super) fn selected_path_mismatch(
    domain: &ValueDomain,
    value: &Value,
    schema: &[(String, bool)],
    response: &[(String, bool)],
    path: &str,
    type_condition: Option<&str>,
) -> Option<String> {
    if value.is_null() && domain.mismatch(value, path).is_none() {
        return None;
    }
    if let Some(condition) = type_condition {
        if graphql_abstract_has_variant(domain, condition)
            && value.get("__typename").and_then(Value::as_str) != Some(condition)
        {
            // A conditional fragment only promises fields for the matching
            // concrete object. Missing or different runtime type evidence is
            // not enough to make a hard selected-field claim.
            return None;
        }
    }
    let domain = concrete_domain(domain, value)?;
    if let ValueDomain::Array { items, .. } = domain {
        let values = value.as_array()?;
        return values.iter().enumerate().find_map(|(index, item)| {
            selected_path_mismatch(
                items,
                item,
                schema,
                response,
                &format!("{path}[{index}]"),
                type_condition,
            )
        });
    }
    let ValueDomain::Object { properties, .. } = domain else {
        return None;
    };
    let ((schema_name, schema_array), schema_rest) = schema.split_first()?;
    let ((response_name, response_array), response_rest) = response.split_first()?;
    if schema_array != response_array {
        return None;
    }
    let field_domain = properties.get(schema_name)?;
    let object = value.as_object()?;
    let Some(field_value) = object.get(response_name) else {
        return Some(format!(
            "{path}.{response_name} was selected by the GraphQL operation but is absent"
        ));
    };
    let field_path = format!("{path}.{response_name}");
    if *schema_array {
        let array_domain = concrete_domain(field_domain, field_value)?;
        let ValueDomain::Array { items, .. } = array_domain else {
            return field_domain.mismatch(field_value, &field_path);
        };
        let Some(values) = field_value.as_array() else {
            return field_domain.mismatch(field_value, &field_path);
        };
        if schema_rest.is_empty() {
            return field_domain.mismatch(field_value, &field_path);
        }
        return values.iter().enumerate().find_map(|(index, item)| {
            selected_path_mismatch(
                items,
                item,
                schema_rest,
                response_rest,
                &format!("{field_path}[{index}]"),
                type_condition,
            )
        });
    }
    if schema_rest.is_empty() {
        field_domain.mismatch(field_value, &field_path)
    } else {
        selected_path_mismatch(
            field_domain,
            field_value,
            schema_rest,
            response_rest,
            &field_path,
            type_condition,
        )
    }
}

fn graphql_abstract_has_variant(domain: &ValueDomain, condition: &str) -> bool {
    match domain {
        ValueDomain::OneOf { variants } => variants
            .iter()
            .any(|variant| graphql_abstract_has_variant(variant, condition)),
        ValueDomain::AllOf { variants } => variants
            .iter()
            .any(|variant| graphql_abstract_has_variant(variant, condition)),
        ValueDomain::GraphqlAbstract { variants } => variants.contains_key(condition),
        _ => false,
    }
}

fn concrete_domain<'a>(domain: &'a ValueDomain, value: &Value) -> Option<&'a ValueDomain> {
    match domain {
        ValueDomain::OneOf { variants } => variants
            .iter()
            .find(|variant| {
                !matches!(variant, ValueDomain::Null) && variant.mismatch(value, "$value").is_none()
            })
            .or_else(|| {
                variants
                    .iter()
                    .find(|variant| !matches!(variant, ValueDomain::Null))
            })
            .and_then(|variant| concrete_domain(variant, value)),
        ValueDomain::AllOf { variants } => variants
            .iter()
            .find(|variant| !matches!(variant, ValueDomain::Any))
            .and_then(|variant| concrete_domain(variant, value))
            .or(Some(domain)),
        ValueDomain::GraphqlAbstract { variants } => value
            .get("__typename")
            .and_then(Value::as_str)
            .and_then(|kind| variants.get(kind))
            .and_then(|variant| concrete_domain(variant, value)),
        _ => Some(domain),
    }
}
