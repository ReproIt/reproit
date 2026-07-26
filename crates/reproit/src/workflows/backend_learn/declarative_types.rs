//! Request-body facts for the three families whose validation is a declaration
//! rather than a type: Ruby, PHP, and Java.
//!
//! I had written these off as "usually nothing declarative to read", which was
//! wrong in all three cases. Rails `validates :x, inclusion: { in: %w[a b] }`,
//! Laravel `'x' => 'required|in:a,b'`, and Bean Validation `@Pattern`/`@Min` are
//! each at least as explicit as a Go struct tag. They share a reader because
//! they share a shape: a named rule set attached to a field.

use super::field_facts::{drop_ambiguous, FieldFact};
use regex::Regex;
use std::collections::{BTreeMap, BTreeSet};

const MAX_TYPE_BYTES: usize = 2 * 1024 * 1024;
const MAX_BLOCK_LINES: usize = 400;

#[derive(Debug, Default)]
pub(super) struct DeclarativeTypes {
    /// class / form-request / controller name -> fields.
    shapes: BTreeMap<String, BTreeMap<String, FieldFact>>,
    /// handler -> the shape validating its body.
    bodies: BTreeMap<String, String>,
}

impl DeclarativeTypes {
    pub(super) fn body_fields(&self, key: &str) -> Option<BTreeMap<String, FieldFact>> {
        if let Some(fields) = self.shapes.get(key) {
            return Some(fields.clone());
        }
        self.shapes.get(self.bodies.get(key)?).cloned()
    }
}

pub(super) struct DeclarativeScanner {
    /// Rails: `validates :name, inclusion: { in: %w[a b] }, numericality: {...}`
    ruby_validates: Regex,
    ruby_inclusion: Regex,
    ruby_word_list: Regex,
    ruby_array_list: Regex,
    ruby_numeric: Regex,
    ruby_class: Regex,
    ruby_def: Regex,
    /// Laravel: `'field' => 'required|in:a,b|min:1|max:5'` and array form.
    php_rule: Regex,
    php_class: Regex,
    /// Bean Validation on a Java field.
    java_field: Regex,
    java_annotation: Regex,
    java_class: Regex,
    java_body_param: Regex,
    java_method: Regex,
    java_enum: Regex,
}

impl DeclarativeScanner {
    pub(super) fn new(compile: impl Fn(&str) -> Regex) -> Self {
        Self {
            ruby_validates: compile(r"^\s*validates\s+:([a-z_]\w*)\s*,(.*)$"),
            ruby_inclusion: compile(r"inclusion:\s*\{\s*in:\s*(.+?)\s*\}"),
            ruby_word_list: compile(r"%[wi]\[([^\]]*)\]"),
            ruby_array_list: compile(r"\[([^\]]*)\]"),
            ruby_numeric: compile(
                r"(greater_than_or_equal_to|less_than_or_equal_to|greater_than|less_than):\s*(-?\d+)",
            ),
            ruby_class: compile(r"^\s*class\s+([A-Z]\w*)"),
            ruby_def: compile(r"^\s*def\s+([a-z_]\w*)"),
            php_rule: compile(r#"['"]([a-z_]\w*)['"]\s*=>\s*(.+?),?\s*$"#),
            php_class: compile(r"^\s*(?:final\s+)?class\s+([A-Za-z_]\w*)"),
            java_field: compile(r"^\s*(?:private|public|protected)\s+\S+\s+([a-z]\w*)\s*[;=]"),
            java_annotation: compile(
                r"@(Min|Max|NotNull|NotBlank|Nullable|Size|Pattern)\s*(?:\(([^)]*)\))?",
            ),
            java_class: compile(r"^\s*(?:public\s+)?(?:final\s+)?class\s+([A-Za-z_]\w*)"),
            java_body_param: compile(r"@RequestBody\s+(?:@Valid\s+)?([A-Za-z_]\w*)"),
            java_method: compile(r"\b(?:public|protected)\s+\S+\s+([a-z]\w*)\s*\("),
            java_enum: compile(r"^\s*(?:public\s+)?enum\s+([A-Za-z_]\w*)"),
        }
    }

    pub(super) fn scan(&self, sources: &[String], family: Family) -> DeclarativeTypes {
        let mut types = DeclarativeTypes::default();
        // These families accumulate a class's fields across lines, so a repeat
        // declaration MERGES rather than replaces. Two same-named classes in
        // different modules would silently blend into one shape nobody wrote,
        // so a name declared in more than one file abstains.
        let mut seen_in: BTreeMap<String, usize> = BTreeMap::new();
        let mut enums: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for source in sources {
            if source.len() > MAX_TYPE_BYTES {
                continue;
            }
            let lines: Vec<&str> = source.lines().collect();
            let before: BTreeSet<String> = types.shapes.keys().cloned().collect();
            match family {
                Family::Ruby => self.ruby(&lines, &mut types),
                Family::Php => self.php(&lines, &mut types),
                Family::Java => self.java(&lines, &mut types, &mut enums),
            }
            for name in types.shapes.keys() {
                if before.contains(name) {
                    continue;
                }
                *seen_in.entry(name.clone()).or_default() += 1;
            }
        }
        let ambiguous: BTreeSet<String> = seen_in
            .into_iter()
            .filter(|(_, files)| *files > 1)
            .map(|(name, _)| name)
            .collect();
        drop_ambiguous(&mut types.shapes, &ambiguous);
        types
    }

    /// Rails model/form validations, attributed to the enclosing class. The
    /// class name is also the controller's usual `Xxx.new(params)` target.
    fn ruby(&self, lines: &[&str], types: &mut DeclarativeTypes) {
        let mut current: Option<String> = None;
        for line in lines {
            if let Some(captures) = self.ruby_class.captures(line) {
                current = Some(captures[1].to_string());
                continue;
            }
            if let Some(captures) = self.ruby_def.captures(line) {
                // A controller action validating an object of the enclosing
                // class: `def create` inside `class BlocksController`.
                if let Some(class) = &current {
                    types.bodies.insert(captures[1].to_string(), class.clone());
                }
                continue;
            }
            let Some(captures) = self.ruby_validates.captures(line) else {
                continue;
            };
            let Some(class) = current.clone() else {
                continue;
            };
            let rules = &captures[2];
            let allowed = self.ruby_inclusion.captures(rules).and_then(|found| {
                self.ruby_word_list
                    .captures(&found[1])
                    .map(|list| {
                        list[1]
                            .split_whitespace()
                            .map(str::to_string)
                            .collect::<Vec<_>>()
                    })
                    .or_else(|| {
                        self.ruby_array_list
                            .captures(&found[1])
                            .and_then(|list| quoted_list(&list[1]))
                    })
            });
            let mut low = None;
            let mut high = None;
            for bound in self.ruby_numeric.captures_iter(rules) {
                let value: f64 = match bound[2].parse() {
                    Ok(value) => value,
                    Err(_) => continue,
                };
                match &bound[1] {
                    "greater_than_or_equal_to" => low = Some(value),
                    "greater_than" => low = Some(value + 1.0),
                    "less_than_or_equal_to" => high = Some(value),
                    "less_than" => high = Some(value - 1.0),
                    _ => {}
                }
            }
            let range = (low.is_some() || high.is_some()).then_some((low, high));
            types.shapes.entry(class).or_default().insert(
                captures[1].to_string(),
                FieldFact {
                    required: rules.contains("presence: true"),
                    evidence: allowed
                        .as_ref()
                        .map(|_| "a validates inclusion rule".to_string())
                        .or_else(|| range.map(|_| "a validates numericality rule".to_string())),
                    allowed,
                    range,
                },
            );
        }
    }

    /// Laravel form-request `rules()` entries, string or array form.
    fn php(&self, lines: &[&str], types: &mut DeclarativeTypes) {
        let mut current: Option<String> = None;
        for line in lines {
            if let Some(captures) = self.php_class.captures(line) {
                current = Some(captures[1].to_string());
                continue;
            }
            let Some(captures) = self.php_rule.captures(line) else {
                continue;
            };
            let Some(class) = current.clone() else {
                continue;
            };
            let raw = &captures[2];
            // Laravel rules are pipe-separated, and `in:` takes a
            // COMMA-separated list inside one rule. Splitting on commas first
            // shreds exactly the list this is here to read.
            let tokens: Vec<&str> = raw
                .trim_matches(|c: char| !c.is_alphanumeric())
                .split('|')
                .map(|token| token.trim().trim_matches(['\'', '"', ' ']))
                .filter(|token| !token.is_empty())
                .collect();
            let allowed = tokens
                .iter()
                .find_map(|token| token.strip_prefix("in:"))
                .map(|values| {
                    values
                        .split(',')
                        .map(|value| value.trim().trim_matches(['\'', '"']).to_string())
                        .filter(|value| !value.is_empty())
                        .collect::<Vec<_>>()
                })
                .filter(|values| values.len() > 1);
            let bound = |prefix: &str| -> Option<f64> {
                tokens
                    .iter()
                    .find_map(|token| token.strip_prefix(prefix))
                    .and_then(|value| value.trim().parse().ok())
            };
            let low = bound("min:");
            let high = bound("max:");
            let range = (low.is_some() || high.is_some()).then_some((low, high));
            types.shapes.entry(class).or_default().insert(
                captures[1].to_string(),
                FieldFact {
                    required: tokens.contains(&"required"),
                    evidence: allowed
                        .as_ref()
                        .map(|_| "a Laravel `in:` rule".to_string())
                        .or_else(|| range.map(|_| "a Laravel min/max rule".to_string())),
                    allowed,
                    range,
                },
            );
        }
    }

    /// Bean Validation annotations on a DTO field, plus `@RequestBody` binding.
    fn java(
        &self,
        lines: &[&str],
        types: &mut DeclarativeTypes,
        enums: &mut BTreeMap<String, Vec<String>>,
    ) {
        let mut current: Option<String> = None;
        let mut pending: Vec<(String, String)> = Vec::new();
        for (index, line) in lines.iter().enumerate() {
            if let Some(captures) = self.java_enum.captures(line) {
                let values: Vec<String> = block(lines, index)
                    .split(',')
                    .map(|value| value.trim().trim_end_matches(';').to_string())
                    .filter(|value| {
                        !value.is_empty()
                            && value.chars().all(|c| c.is_ascii_uppercase() || c == '_')
                    })
                    .collect();
                if !values.is_empty() {
                    enums.insert(captures[1].to_string(), values);
                }
                continue;
            }
            if let Some(captures) = self.java_class.captures(line) {
                current = Some(captures[1].to_string());
                continue;
            }
            if let Some(captures) = self.java_body_param.captures(line) {
                let model = captures[1].to_string();
                if let Some(method) = self.java_method.captures(line) {
                    types.bodies.insert(method[1].to_string(), model);
                }
                continue;
            }
            for captures in self.java_annotation.captures_iter(line) {
                pending.push((
                    captures[1].to_string(),
                    captures
                        .get(2)
                        .map_or(String::new(), |a| a.as_str().to_string()),
                ));
            }
            let Some(captures) = self.java_field.captures(line) else {
                continue;
            };
            let Some(class) = current.clone() else {
                continue;
            };
            let mut fact = FieldFact {
                required: false,
                ..FieldFact::default()
            };
            let mut low = None;
            let mut high = None;
            for (name, argument) in pending.drain(..) {
                match name.as_str() {
                    "Min" => low = numeric_argument(&argument),
                    "Max" => high = numeric_argument(&argument),
                    "NotNull" | "NotBlank" => fact.required = true,
                    _ => {}
                }
            }
            if low.is_some() || high.is_some() {
                fact.range = Some((low, high));
                fact.evidence = Some("a @Min/@Max constraint".to_string());
            }
            // An enum-typed field is a closed value set, same as everywhere else.
            if let Some((_, values)) = enums.iter().find(|(name, _)| line.contains(name.as_str())) {
                fact.allowed = Some(values.clone());
                fact.evidence = Some("an enum-typed field".to_string());
            }
            types
                .shapes
                .entry(class)
                .or_default()
                .insert(captures[1].to_string(), fact);
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
pub(super) enum Family {
    Ruby,
    Php,
    Java,
}

fn numeric_argument(argument: &str) -> Option<f64> {
    argument
        .split('=')
        .next_back()?
        .trim()
        .trim_end_matches(['L', 'l'])
        .parse()
        .ok()
}

fn quoted_list(inner: &str) -> Option<Vec<String>> {
    let mut values = Vec::new();
    for part in inner.split(',') {
        let item = part.trim();
        if item.is_empty() {
            continue;
        }
        let unquoted = item.trim_matches(['\'', '"']);
        if unquoted == item {
            return None;
        }
        values.push(unquoted.to_string());
    }
    (values.len() > 1).then_some(values)
}

fn block(lines: &[&str], open: usize) -> String {
    let end = (open + MAX_BLOCK_LINES).min(lines.len());
    let joined = lines[open..end].join("\n");
    let Some(start) = joined.find('{') else {
        return String::new();
    };
    let mut depth = 0i32;
    for (index, character) in joined[start..].char_indices() {
        match character {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return joined[start + 1..start + index].to_string();
                }
            }
            _ => {}
        }
    }
    String::new()
}

pub(super) fn scan_types(root: &std::path::Path, family: Family) -> DeclarativeTypes {
    let extract_family = match family {
        Family::Ruby => super::extract::Family::Ruby,
        Family::Php => super::extract::Family::Php,
        Family::Java => super::extract::Family::Spring,
    };
    let scanner =
        DeclarativeScanner::new(|pattern| Regex::new(pattern).expect("static declarative pattern"));
    let sources: Vec<String> = super::extract::family_sources(root, extract_family)
        .into_iter()
        .filter_map(|file| std::fs::read_to_string(file).ok())
        .collect();
    scanner.scan(&sources, family)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan(sources: &[&str], family: Family) -> DeclarativeTypes {
        DeclarativeScanner::new(|p| Regex::new(p).expect("pattern")).scan(
            &sources.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            family,
        )
    }

    #[test]
    fn rails_inclusion_and_numericality_are_read() {
        let types = scan(
            &[r#"
class Block < ApplicationRecord
  validates :blocked_type, presence: true, inclusion: { in: %w[user sponsor] }
  validates :rating, numericality: { greater_than_or_equal_to: -1, less_than_or_equal_to: 1 }
end
"#],
            Family::Ruby,
        );
        let fields = types.body_fields("Block").expect("class is a key");
        assert_eq!(
            fields["blocked_type"].allowed.as_deref(),
            Some(["user".to_string(), "sponsor".to_string()].as_slice())
        );
        assert!(fields["blocked_type"].required);
        assert_eq!(fields["rating"].range, Some((Some(-1.0), Some(1.0))));
    }

    #[test]
    fn laravel_rules_are_read() {
        let types = scan(
            &[r#"
class StoreBlockRequest extends FormRequest
{
    public function rules()
    {
        return [
            'blocked_type' => 'required|in:user,sponsor',
            'rating' => 'integer|min:-1|max:1',
            'note' => 'nullable|string',
        ];
    }
}
"#],
            Family::Php,
        );
        let fields = types
            .body_fields("StoreBlockRequest")
            .expect("class is a key");
        assert_eq!(
            fields["blocked_type"].allowed.as_deref(),
            Some(["user".to_string(), "sponsor".to_string()].as_slice())
        );
        assert!(fields["blocked_type"].required);
        assert_eq!(fields["rating"].range, Some((Some(-1.0), Some(1.0))));
        assert!(!fields["note"].required);
    }

    #[test]
    fn bean_validation_constraints_are_read() {
        let types = scan(
            &[r#"
public class BlockRequest {
    @NotNull
    private String blockedId;

    @Min(-1)
    @Max(1)
    private int rating;

    private String note;
}
"#],
            Family::Java,
        );
        let fields = types.body_fields("BlockRequest").expect("class is a key");
        assert!(fields["blockedId"].required);
        assert_eq!(fields["rating"].range, Some((Some(-1.0), Some(1.0))));
        assert!(!fields["note"].required);
        assert_eq!(fields["note"].allowed, None);
    }

    #[test]
    fn a_rails_field_with_no_readable_rule_stays_open() {
        let types = scan(
            &["class B < ApplicationRecord\n  validates :name, presence: true\nend"],
            Family::Ruby,
        );
        let fields = types.body_fields("B").unwrap();
        assert_eq!(fields["name"].allowed, None);
        assert_eq!(fields["name"].range, None);
        assert!(fields["name"].required);
    }

    #[test]
    fn a_laravel_rule_with_a_single_in_value_is_not_a_set() {
        let types = scan(
            &["class R extends FormRequest\n{\n  return ['x' => 'required|in:only'];\n}"],
            Family::Php,
        );
        assert_eq!(types.body_fields("R").unwrap()["x"].allowed, None);
    }
}
