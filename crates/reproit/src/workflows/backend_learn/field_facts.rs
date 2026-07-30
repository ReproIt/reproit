//! Field facts shared by every family's source reader.
//!
//! `FieldFact` is the one vocabulary all of them speak: what a request body
//! field accepts, whether it is required, and what evidence says so. The
//! helpers here are the pieces more than one reader needs.

use std::collections::{BTreeMap, BTreeSet};

/// What the code says about one request-body field.
#[derive(Debug, Clone, Default, PartialEq)]
pub(super) struct FieldFact {
    /// Non-`Option`, so the handler will reject a body that omits it.
    pub(super) required: bool,
    /// The exact values accepted, serde renames applied. From a unit-only enum,
    /// or from an explicit literal-set guard in the handler. None when the
    /// accepted set is open or could not be read.
    pub(super) allowed: Option<Vec<String>>,
    /// Inclusive numeric bounds the handler enforces, from a `validate`/`garde`
    /// range attribute or an explicit range guard. Either side may be open.
    pub(super) range: Option<(Option<f64>, Option<f64>)>,
    /// Where the constraint came from, so the report can name the evidence
    /// rather than assert a bare fact.
    pub(super) evidence: Option<String>,
}
/// Record a declaration, treating a CONFLICTING redefinition as ambiguous.
///
/// Every language here namespaces types by module; these scanners key them by
/// bare name, so two modules declaring the same name silently overwrote each
/// other and which one won depended on directory walk order. That is how a
/// report came to tell someone three fields of a live struct did not exist.
/// An ambiguous name is dropped rather than resolved to a guess: not knowing
/// which type a handler takes has to read as "not checked", never as a verdict.
pub(super) fn record<T: PartialEq>(
    map: &mut BTreeMap<String, T>,
    ambiguous: &mut BTreeSet<String>,
    name: String,
    value: T,
) {
    match map.get(&name) {
        Some(existing) if *existing != value => {
            ambiguous.insert(name);
        }
        Some(_) => {}
        None => {
            map.insert(name, value);
        }
    }
}
/// Drop every name that had conflicting declarations.
pub(super) fn drop_ambiguous<T>(map: &mut BTreeMap<String, T>, ambiguous: &BTreeSet<String>) {
    for name in ambiguous {
        map.remove(name);
    }
}
/// Inclusive bounds from a `validator` or `garde` range attribute.
///
/// A Rust numeric type carries no range, but these attributes state one
/// declaratively, which is exactly as readable as the enum case and just as
/// unambiguous. `#[validate(range(min = -1, max = 1))]` against a schema
/// declaring `1..5` is a contradiction the compiler-adjacent source already
/// spells out.
pub(super) fn attribute_range(attributes: &[String]) -> Option<(Option<f64>, Option<f64>)> {
    // Attribute text arrives either from a line of source or from parser
    // tokens, and the parser spaces everything out (`range (min = - 1 ,`).
    // Collapsing whitespace makes one reader serve both.
    let text: String = attributes
        .join(" ")
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect();
    if !text.contains("range(") {
        return None;
    }
    let bound = |key: &str| -> Option<f64> {
        let at = text.find(key)? + key.len();
        let rest = text[at..].trim_start().strip_prefix('=')?.trim_start();
        let literal: String = rest
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '-' || *c == '.')
            .collect();
        literal.parse().ok()
    };
    let min = bound("min");
    let max = bound("max");
    (min.is_some() || max.is_some()).then_some((min, max))
}
/// The payload type a handler accepts, reduced to the bare name shape keys use:
/// generics unwrapped (`ResponseEntity<Widget>` and `List<Widget>` both name `Widget`),
/// the package or namespace qualifier dropped, and array suffixes stripped. Java and
/// C# once had separate copies; the C# one kept generics intact, so a generic
/// `[FromBody]` type could never match its declaration.
pub(super) fn bare_type(raw: &str) -> String {
    let inner = raw
        .split_once('<')
        .and_then(|(_, rest)| rest.rsplit_once('>'))
        .map(|(inner, _)| inner)
        .unwrap_or(raw);
    inner
        .rsplit('.')
        .next()
        .unwrap_or(inner)
        .trim()
        .trim_end_matches("[]")
        .to_string()
}

/// The members of a comma-separated literal list (`"a", "b"`), or None when any
/// member is not a literal: a variable in the list means the accepted set is not
/// knowable from this expression. `quotes` names the language's string delimiters
/// (JS adds the backtick), and `bare_numbers` accepts unquoted numeric members
/// where the language's idiom uses them. Fewer than two members is not a set.
pub(super) fn literal_values(
    inner: &str,
    quotes: &[char],
    bare_numbers: bool,
) -> Option<Vec<String>> {
    let mut values = Vec::new();
    for part in inner.split(',') {
        let item = part.trim();
        if item.is_empty() {
            continue;
        }
        let unquoted = item.trim_matches(quotes);
        if unquoted == item && !(bare_numbers && item.parse::<f64>().is_ok()) {
            return None;
        }
        values.push(unquoted.to_string());
    }
    (values.len() > 1).then_some(values)
}

pub(super) fn apply_rename_all(variant: &str, rule: Option<&str>) -> String {
    match rule {
        Some("snake_case") => to_snake(variant),
        Some("SCREAMING_SNAKE_CASE") => to_snake(variant).to_uppercase(),
        Some("kebab-case") => to_snake(variant).replace('_', "-"),
        Some("lowercase") => variant.to_lowercase(),
        Some("UPPERCASE") => variant.to_uppercase(),
        Some("camelCase") => {
            let snake = to_snake(variant);
            let mut parts = snake.split('_');
            let first = parts.next().unwrap_or("").to_string();
            first
                + &parts
                    .map(|part| {
                        let mut chars = part.chars();
                        match chars.next() {
                            Some(head) => head.to_uppercase().to_string() + chars.as_str(),
                            None => String::new(),
                        }
                    })
                    .collect::<String>()
        }
        // No rule: serde keeps the variant name verbatim.
        _ => variant.to_string(),
    }
}
fn to_snake(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 4);
    for (index, character) in value.chars().enumerate() {
        if character.is_uppercase() {
            if index > 0 {
                out.push('_');
            }
            out.extend(character.to_lowercase());
        } else {
            out.push(character);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rename_all_rules_map_as_serde_does() {
        assert_eq!(
            apply_rename_all("FastPath", Some("snake_case")),
            "fast_path"
        );
        assert_eq!(
            apply_rename_all("FastPath", Some("kebab-case")),
            "fast-path"
        );
        assert_eq!(apply_rename_all("FastPath", Some("camelCase")), "fastPath");
        assert_eq!(apply_rename_all("FastPath", None), "FastPath");
    }

    #[test]
    fn a_conflicting_redeclaration_is_dropped_and_an_identical_one_is_not() {
        let mut map = BTreeMap::new();
        let mut ambiguous = BTreeSet::new();
        record(&mut map, &mut ambiguous, "same".into(), 1);
        record(&mut map, &mut ambiguous, "same".into(), 1);
        record(&mut map, &mut ambiguous, "clash".into(), 1);
        record(&mut map, &mut ambiguous, "clash".into(), 2);
        drop_ambiguous(&mut map, &ambiguous);
        assert!(map.contains_key("same"), "identical is one declaration");
        assert!(!map.contains_key("clash"), "a conflict must abstain");
    }

    #[test]
    fn a_validator_range_attribute_is_read() {
        let attrs = vec!["validate (range (min = -1, max = 1))".to_string()];
        assert_eq!(attribute_range(&attrs), Some((Some(-1.0), Some(1.0))));
        assert_eq!(attribute_range(&["serde (default)".to_string()]), None);
    }
}
