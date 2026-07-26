//! Request-body facts read from Go handler signatures.
//!
//! Go states its constraints in struct tags, which is the most declarative of
//! the three families: `binding:"required,oneof=user sponsor"` is a closed value
//! set and a required flag in one place, and gin, echo and fiber all read the
//! same `validate`/`binding` vocabulary.

use super::rust_types::{drop_ambiguous, record, FieldFact};
use regex::Regex;
use std::collections::{BTreeMap, BTreeSet};

const MAX_TYPE_BYTES: usize = 2 * 1024 * 1024;

#[derive(Debug, Default)]
pub(super) struct GoTypes {
    /// handler fn -> bound struct name.
    bodies: BTreeMap<String, String>,
    /// struct name -> json field name -> fact.
    structs: BTreeMap<String, BTreeMap<String, FieldFact>>,
}

impl GoTypes {
    pub(super) fn body_fields(&self, handler: &str) -> Option<BTreeMap<String, FieldFact>> {
        self.structs.get(self.bodies.get(handler)?).cloned()
    }
}

pub(super) struct GoScanner {
    struct_open: Regex,
    field: Regex,
    json_tag: Regex,
    rules: Regex,
    func_open: Regex,
    bind: Regex,
    declare: Regex,
}

impl GoScanner {
    pub(super) fn new(compile: impl Fn(&str) -> Regex) -> Self {
        Self {
            struct_open: compile(r"^\s*type\s+([A-Za-z_]\w*)\s+struct\s*\{"),
            field: compile(r#"^\s*([A-Z]\w*)\s+([^\s`]+)\s*`([^`]*)`"#),
            json_tag: compile(r#"json:"([^",]+)"#),
            rules: compile(r#"(?:binding|validate):"([^"]*)""#),
            func_open: compile(r"^\s*func\s+(?:\([^)]*\)\s*)?([A-Za-z_]\w*)\s*\("),
            // gin `c.ShouldBindJSON(&req)`, echo `c.Bind(&req)`, fiber `c.BodyParser(&req)`.
            bind: compile(r"(?:ShouldBindJSON|BindJSON|BodyParser|Bind)\(\s*&\s*([A-Za-z_]\w*)"),
            declare: compile(
                r"\bvar\s+([A-Za-z_]\w*)\s+([A-Za-z_]\w*)|\b([A-Za-z_]\w*)\s*:=\s*([A-Za-z_]\w*)\{",
            ),
        }
    }

    pub(super) fn scan(&self, sources: &[String]) -> GoTypes {
        let mut types = GoTypes::default();
        let mut ambiguous_structs = BTreeSet::new();
        let mut ambiguous_bodies = BTreeSet::new();
        for source in sources {
            if source.len() > MAX_TYPE_BYTES {
                continue;
            }
            self.scan_one(
                source,
                &mut types,
                &mut ambiguous_structs,
                &mut ambiguous_bodies,
            );
        }
        drop_ambiguous(&mut types.structs, &ambiguous_structs);
        drop_ambiguous(&mut types.bodies, &ambiguous_bodies);
        types
    }

    fn scan_one(
        &self,
        source: &str,
        types: &mut GoTypes,
        ambiguous_structs: &mut BTreeSet<String>,
        ambiguous_bodies: &mut BTreeSet<String>,
    ) {
        let lines: Vec<&str> = source.lines().map(strip_comment).collect();
        for (index, line) in lines.iter().enumerate() {
            if let Some(captures) = self.struct_open.captures(line) {
                let fields = self.struct_fields(&lines, index);
                if !fields.is_empty() {
                    record(
                        &mut types.structs,
                        ambiguous_structs,
                        captures[1].to_string(),
                        fields,
                    );
                }
                continue;
            }
            if let Some(captures) = self.func_open.captures(line) {
                if let Some(bound) = self.bound_struct(&lines, index) {
                    record(
                        &mut types.bodies,
                        ambiguous_bodies,
                        captures[1].to_string(),
                        bound,
                    );
                }
            }
        }
    }

    fn struct_fields(&self, lines: &[&str], open: usize) -> BTreeMap<String, FieldFact> {
        let mut fields = BTreeMap::new();
        for line in lines.iter().skip(open + 1) {
            if line.trim_start().starts_with('}') {
                break;
            }
            let Some(captures) = self.field.captures(line) else {
                continue;
            };
            let tags = &captures[3];
            // Only fields the JSON body actually names.
            let Some(name) = self
                .json_tag
                .captures(tags)
                .map(|json| json[1].to_string())
                .filter(|name| name != "-")
            else {
                continue;
            };
            let rules = self
                .rules
                .captures(tags)
                .map(|rules| rules[1].to_string())
                .unwrap_or_default();
            let pointer = captures[2].starts_with('*');
            let allowed = rules.split(',').find_map(|rule| {
                rule.trim()
                    .strip_prefix("oneof=")
                    .map(|values| values.split_whitespace().map(str::to_string).collect())
            });
            let range = numeric_rule(&rules);
            fields.insert(
                name,
                FieldFact {
                    // `omitempty` and a pointer both mean the body may omit it.
                    required: rules.split(',').any(|rule| rule.trim() == "required")
                        || (!pointer && !tags.contains("omitempty")),
                    evidence: allowed
                        .as_ref()
                        .map(|_| "a struct tag `oneof` rule".to_string())
                        .or_else(|| range.map(|_| "a struct tag min/max rule".to_string())),
                    allowed,
                    range,
                },
            );
        }
        fields
    }

    /// The struct a handler binds its request body into.
    fn bound_struct(&self, lines: &[&str], at: usize) -> Option<String> {
        let end = (at + 40).min(lines.len());
        let body = lines[at..end].join("\n");
        let variable = self.bind.captures(&body)?[1].to_string();
        // Resolve the variable back to its declared type.
        self.declare.captures_iter(&body).find_map(|captures| {
            let (name, kind) = match (captures.get(1), captures.get(3)) {
                (Some(name), _) => (name.as_str(), captures.get(2)?.as_str()),
                (None, Some(name)) => (name.as_str(), captures.get(4)?.as_str()),
                _ => return None,
            };
            (name == variable).then(|| kind.to_string())
        })
    }
}

/// `min=1,max=5` and gte/lte, the numeric half of the validate vocabulary.
fn numeric_rule(rules: &str) -> Option<(Option<f64>, Option<f64>)> {
    let mut low = None;
    let mut high = None;
    for rule in rules.split(',') {
        let rule = rule.trim();
        for (prefix, slot) in [
            ("min=", true),
            ("gte=", true),
            ("max=", false),
            ("lte=", false),
        ] {
            if let Some(value) = rule
                .strip_prefix(prefix)
                .and_then(|v| v.parse::<f64>().ok())
            {
                if slot {
                    low = Some(value);
                } else {
                    high = Some(value);
                }
            }
        }
    }
    (low.is_some() || high.is_some()).then_some((low, high))
}

fn strip_comment(line: &str) -> &str {
    match line.find("//") {
        Some(at) => &line[..at],
        None => line,
    }
}

pub(super) fn scan_types(root: &std::path::Path) -> GoTypes {
    let scanner = GoScanner::new(|pattern| Regex::new(pattern).expect("static go pattern"));
    let sources: Vec<String> = super::extract::family_sources(root, super::extract::Family::Go)
        .into_iter()
        .filter_map(|file| std::fs::read_to_string(file).ok())
        .collect();
    scanner.scan(&sources)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan(sources: &[&str]) -> GoTypes {
        GoScanner::new(|p| Regex::new(p).expect("pattern"))
            .scan(&sources.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    const APP: &str = r#"
type BlockRequest struct {
	BlockedType string  `json:"blocked_type" binding:"required,oneof=user sponsor"`
	BlockedID   string  `json:"blocked_id" binding:"required"`
	Rating      int     `json:"rating" binding:"min=-1,max=1"`
	Note        *string `json:"note,omitempty"`
}

func CreateBlock(c *gin.Context) {
	var req BlockRequest
	if err := c.ShouldBindJSON(&req); err != nil {
		return
	}
}
"#;

    #[test]
    fn a_oneof_tag_is_a_closed_value_set() {
        let fields = scan(&[APP]).body_fields("CreateBlock").expect("resolved");
        assert_eq!(
            fields["blocked_type"].allowed.as_deref(),
            Some(["user".to_string(), "sponsor".to_string()].as_slice())
        );
        assert!(fields["blocked_type"].required);
    }

    #[test]
    fn min_max_tags_are_a_range() {
        let fields = scan(&[APP]).body_fields("CreateBlock").unwrap();
        assert_eq!(fields["rating"].range, Some((Some(-1.0), Some(1.0))));
    }

    #[test]
    fn a_pointer_or_omitempty_field_is_optional() {
        let fields = scan(&[APP]).body_fields("CreateBlock").unwrap();
        assert!(!fields["note"].required);
        assert!(fields["blocked_id"].required);
    }

    #[test]
    fn fields_are_keyed_by_their_json_name_not_the_go_name() {
        let fields = scan(&[APP]).body_fields("CreateBlock").unwrap();
        assert!(fields.contains_key("blocked_type"), "{:?}", fields.keys());
        assert!(!fields.contains_key("BlockedType"));
    }

    #[test]
    fn a_field_with_no_rules_stays_open() {
        let fields = scan(&[APP]).body_fields("CreateBlock").unwrap();
        assert_eq!(fields["blocked_id"].allowed, None);
        assert_eq!(fields["blocked_id"].range, None);
    }

    #[test]
    fn a_handler_that_binds_nothing_resolves_no_body() {
        let types = scan(&["func List(c *gin.Context) {\n\tc.JSON(200, rows)\n}"]);
        assert!(types.body_fields("List").is_none());
    }
}
