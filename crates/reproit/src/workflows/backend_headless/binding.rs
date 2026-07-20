use super::*;

#[derive(Default)]
pub(super) struct ValueBank {
    values: BTreeMap<String, Vec<Value>>,
}

impl ValueBank {
    pub(super) fn harvest(&mut self, value: &Value) {
        self.harvest_named(value, None);
    }

    fn harvest_named(&mut self, value: &Value, name: Option<&str>) {
        match value {
            Value::Object(object) => {
                for (key, value) in object {
                    self.harvest_named(value, Some(key));
                }
            }
            Value::Array(values) => {
                for value in values {
                    self.harvest_named(value, name);
                }
            }
            Value::String(_) | Value::Number(_) | Value::Bool(_) => {
                let Some(name) = name else {
                    return;
                };
                let normalized = normalized_name(name);
                if !is_bindable_name(&normalized) {
                    return;
                }
                self.values
                    .entry(normalized.clone())
                    .or_default()
                    .push(value.clone());
                if normalized != "id" && normalized.ends_with("id") {
                    self.values
                        .entry("id".into())
                        .or_default()
                        .push(value.clone());
                }
            }
            Value::Null => {}
        }
    }

    pub(super) fn bind(&self, domain: &ValueDomain, value: &mut Value, name: Option<&str>) {
        match (domain, value) {
            (ValueDomain::Object { properties, .. }, Value::Object(current)) => {
                for (property, property_domain) in properties {
                    if let Some(property_value) = current.get_mut(property) {
                        self.bind(property_domain, property_value, Some(property));
                    }
                }
            }
            (ValueDomain::Array { items, .. }, Value::Array(current)) => {
                for item in current {
                    self.bind(items, item, name);
                }
            }
            (ValueDomain::OneOf { variants }, current) => {
                if let Some(variant) = variants
                    .iter()
                    .find(|variant| variant.mismatch(current, "$candidate").is_none())
                {
                    self.bind(variant, current, name);
                }
            }
            (ValueDomain::AllOf { variants }, current) => {
                for variant in variants {
                    self.bind(variant, current, name);
                }
            }
            (domain, current) => {
                let Some(name) = name else {
                    return;
                };
                let normalized = normalized_name(name);
                if !is_bindable_name(&normalized) {
                    return;
                }
                let candidates = self.values.get(&normalized).or_else(|| {
                    normalized
                        .ends_with("id")
                        .then(|| self.values.get("id"))
                        .flatten()
                });
                if let Some(candidate) = candidates
                    .into_iter()
                    .flatten()
                    .find(|candidate| domain.mismatch(candidate, "$candidate").is_none())
                {
                    *current = candidate.clone();
                }
            }
        }
    }
}

fn normalized_name(name: &str) -> String {
    name.chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn is_bindable_name(name: &str) -> bool {
    name == "id" || name.ends_with("id") || matches!(name, "slug" | "code")
}
