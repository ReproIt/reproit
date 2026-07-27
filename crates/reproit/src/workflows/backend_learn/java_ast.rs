//! Spring route and Bean Validation extraction over the grammar.
//!
//! Java is the family the pattern reader served worst, and for a structural
//! reason: everything here is an ANNOTATION attached to the declaration below
//! it. `@GetMapping("/x")` on the line before a method, `@Min(-1)` and `@Max(1)`
//! stacked above a field, `@RequestBody BlockRequest body` inside a parameter
//! list. The pattern reader recovered the attachment by looking a few lines
//! ahead, which is a guess that a blank line or an intervening annotation
//! breaks. Over a parse the annotation is a child of the thing it annotates.
//!
//! The other thing this buys is the class-level `@RequestMapping` prefix, which
//! is only the prefix for methods in THAT class body, not for every mapping
//! that happens to follow it in the file.

use super::extract::Family;
use super::field_facts::{apply_rename_all, drop_ambiguous, record, FieldFact};
use super::grammar::{self, SourceRead, MAX_FIELDS};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use tree_sitter::Node;

/// The Spring mapping annotations, and the verb each states.
const MAPPINGS: [(&str, &str); 5] = [
    ("GetMapping", "get"),
    ("PostMapping", "post"),
    ("PutMapping", "put"),
    ("PatchMapping", "patch"),
    ("DeleteMapping", "delete"),
];

pub(super) fn read(root: &Path) -> SourceRead {
    let mut source = SourceRead::default();
    // type name -> its Bean Validation constrained fields.
    let mut shapes: BTreeMap<String, BTreeMap<String, FieldFact>> = BTreeMap::new();
    let mut ambiguous: BTreeSet<String> = BTreeSet::new();
    // enum name -> its constants, so an enum-typed field is a closed value set.
    let mut enums: BTreeMap<String, Vec<String>> = BTreeMap::new();
    // handler method -> the `@RequestBody` type it accepts.
    let mut handler_body: BTreeMap<String, String> = BTreeMap::new();
    // (type, wire field) -> the enum type it is declared as, resolved once
    // every file has been read: the constants live in another declaration.
    let mut pending: Vec<(String, String, String)> = Vec::new();
    let mut found: Vec<(String, &'static str, Option<String>)> = Vec::new();

    grammar::read_files(
        root,
        Family::Spring,
        tree_sitter_java::LANGUAGE.into(),
        &mut source,
        |root_node, text| {
            grammar::walk(root_node, &mut |node| match node.kind() {
                "class_declaration" | "record_declaration" => {
                    collect_class(
                        node,
                        text,
                        &mut shapes,
                        &mut ambiguous,
                        &mut handler_body,
                        &mut found,
                        &mut pending,
                    );
                }
                "enum_declaration" => collect_enum(node, text, &mut enums),
                _ => {}
            });
        },
    );
    drop_ambiguous(&mut shapes, &ambiguous);
    // An enum-typed field's accepted set is the enum's constants. A type that
    // is not a known enum stays open rather than becoming an empty set: not
    // finding the declaration is not evidence that the field accepts nothing.
    for (owner, field, enum_type) in pending {
        let Some(values) = enums.get(&enum_type).filter(|values| values.len() > 1) else {
            continue;
        };
        if let Some(fact) = shapes.get_mut(&owner).and_then(|f| f.get_mut(&field)) {
            fact.allowed = Some(values.clone());
            fact.evidence = Some("an enum-typed field".to_string());
        }
    }

    for (path, method, handler) in found {
        source.routes.push((path, method, handler.clone()));
        if let Some(handler) = handler {
            if let Some(fields) = handler_body
                .get(&handler)
                .and_then(|name| shapes.get(name))
                .cloned()
            {
                source.bodies.insert(handler, fields);
            }
        }
    }
    source
}

/// One class: its mapping prefix, its handler methods, and its own fields.
///
/// The prefix applies to this class body only. A file holding a controller and
/// a DTO used to leak the controller's `@RequestMapping` onto the DTO.
fn collect_class(
    node: Node,
    text: &str,
    shapes: &mut BTreeMap<String, BTreeMap<String, FieldFact>>,
    ambiguous: &mut BTreeSet<String>,
    handler_body: &mut BTreeMap<String, String>,
    found: &mut Vec<(String, &'static str, Option<String>)>,
    pending: &mut Vec<(String, String, String)>,
) {
    let Some(name) = grammar::field(node, text, "name") else {
        return;
    };
    let annotations = annotations_of(node, text);
    let prefix = annotations
        .iter()
        .find(|(name, _)| name == "RequestMapping")
        .and_then(|(_, argument)| argument.clone())
        .unwrap_or_default();
    let Some(body) = node.child_by_field_name("body") else {
        return;
    };
    let mut members = Vec::new();
    grammar::children(body, &mut members);
    let mut fields = BTreeMap::new();
    for member in members {
        match member.kind() {
            "method_declaration" => {
                collect_method(member, text, &prefix, handler_body, found);
            }
            "field_declaration" => collect_field(member, text, &name, &mut fields, pending),
            _ => {}
        }
    }
    // A record states its components as parameters, not as fields.
    if let Some(parameters) = node.child_by_field_name("parameters") {
        let mut components = Vec::new();
        grammar::children(parameters, &mut components);
        for component in components {
            collect_field(component, text, &name, &mut fields, pending);
        }
    }
    if !fields.is_empty() {
        record(shapes, ambiguous, name, fields);
    }
}

/// `@PostMapping("/blocks") ResponseEntity<Void> createBlock(@RequestBody T b)`
fn collect_method(
    node: Node,
    text: &str,
    prefix: &str,
    handler_body: &mut BTreeMap<String, String>,
    found: &mut Vec<(String, &'static str, Option<String>)>,
) -> Option<()> {
    let handler = grammar::field(node, text, "name")?;
    let annotations = annotations_of(node, text);
    let mapping = annotations.iter().find_map(|(name, argument)| {
        MAPPINGS
            .into_iter()
            .find(|(mapping, _)| mapping == name)
            .map(|(_, verb)| (verb, argument.clone()))
    });
    // `@RequestMapping(method = RequestMethod.PUT)` states its verb in an
    // argument rather than in the annotation name. Without a method it serves
    // every verb, and claiming one would be an invention, so GET is taken as
    // the one a draft can exercise without mutating anything.
    let (verb, argument) = match mapping {
        Some(found) => found,
        None => {
            let request = annotations
                .iter()
                .find(|(name, _)| name == "RequestMapping")?;
            (request_verb(node, text).unwrap_or("get"), request.1.clone())
        }
    };
    // Spring templates are as often relative as absolute (`@GetMapping("owners")`).
    // Treating a relative one as absent collapsed it onto the class prefix,
    // inventing a route at the prefix and losing the real path.
    let path = match argument {
        Some(path) => join(prefix, &path),
        // A bare `@GetMapping` maps the prefix itself.
        None => join(prefix, ""),
    };
    if let Some(parameters) = node.child_by_field_name("parameters") {
        let mut params = Vec::new();
        grammar::children(parameters, &mut params);
        for parameter in params {
            let takes_body = annotations_of(parameter, text)
                .iter()
                .any(|(name, _)| name == "RequestBody");
            if takes_body {
                if let Some(ty) = grammar::field(parameter, text, "type") {
                    handler_body.insert(handler.clone(), bare_type(&ty));
                }
            }
        }
    }
    found.push((path, verb, Some(handler)));
    Some(())
}

/// The verb a `@RequestMapping(method = RequestMethod.PUT)` names.
fn request_verb(node: Node, text: &str) -> Option<&'static str> {
    let raw = grammar::text(node, text);
    let at = raw.find("RequestMethod.")? + "RequestMethod.".len();
    let verb: String = raw[at..]
        .chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .collect::<String>()
        .to_ascii_lowercase();
    MAPPINGS
        .into_iter()
        .map(|(_, known)| known)
        .find(|known| *known == verb)
}

/// Compose a class template with a method template, either of which may be
/// written with or without slashes.
fn join(prefix: &str, suffix: &str) -> String {
    let prefix = prefix.trim_matches('/');
    let suffix = suffix.trim_matches('/');
    match (prefix.is_empty(), suffix.is_empty()) {
        (true, true) => "/".to_string(),
        (true, false) => format!("/{suffix}"),
        (false, true) => format!("/{prefix}"),
        (false, false) => format!("/{prefix}/{suffix}"),
    }
}

/// A field or record component, with whatever its annotations constrain.
fn collect_field(
    node: Node,
    text: &str,
    owner: &str,
    fields: &mut BTreeMap<String, FieldFact>,
    pending: &mut Vec<(String, String, String)>,
) {
    if fields.len() >= MAX_FIELDS {
        return;
    }
    let name = match node.kind() {
        // `private String blockedType;`
        "field_declaration" => node
            .child_by_field_name("declarator")
            .and_then(|declarator| grammar::field(declarator, text, "name")),
        _ => grammar::field(node, text, "name"),
    };
    let (Some(name), Some(ty)) = (name, grammar::field(node, text, "type")) else {
        return;
    };
    let annotations = annotations_of(node, text);
    let mut fact = FieldFact::default();
    let bare = bare_type(&ty);
    // Only a declared constraint makes a field required. A primitive is not a
    // statement about the request: Jackson defaults an absent `int` to zero
    // rather than rejecting the body.
    for (annotation, argument) in &annotations {
        match annotation.as_str() {
            "NotNull" | "NotBlank" | "NotEmpty" => fact.required = true,
            "Min" | "DecimalMin" => {
                let low = argument.as_deref().and_then(grammar::number);
                fact.range = Some((low, fact.range.and_then(|(_, high)| high)));
                fact.evidence = Some("a @Min/@Max constraint".to_string());
            }
            "Max" | "DecimalMax" => {
                let high = argument.as_deref().and_then(grammar::number);
                fact.range = Some((fact.range.and_then(|(low, _)| low), high));
                fact.evidence = Some("a @Min/@Max constraint".to_string());
            }
            _ => {}
        }
    }
    // `@JsonProperty("blocked_type")` renames the wire field, and comparing the
    // Java name against a snake_case schema is how a present field reads as
    // absent.
    let wire = annotations
        .iter()
        .find(|(name, _)| name == "JsonProperty")
        .and_then(|(_, argument)| argument.clone())
        .unwrap_or_else(|| apply_rename_all(&name, None));
    // An enum type names a closed set, but the constants live in another
    // declaration, so the resolution waits until every file has been read.
    if !is_builtin(&bare) {
        pending.push((owner.to_string(), wire.clone(), bare));
    }
    fields.insert(wire, fact);
}

fn collect_enum(node: Node, text: &str, enums: &mut BTreeMap<String, Vec<String>>) {
    let (Some(name), Some(body)) = (
        grammar::field(node, text, "name"),
        node.child_by_field_name("body"),
    ) else {
        return;
    };
    let mut constants = Vec::new();
    grammar::walk(body, &mut |inner| {
        if inner.kind() == "enum_constant" {
            if let Some(constant) = grammar::field(inner, text, "name") {
                constants.push(constant);
            }
        }
    });
    if !constants.is_empty() {
        enums.insert(name, constants);
    }
}

/// The annotations on a declaration, each with its single literal argument.
///
/// Only a lone literal is read. `@RequestMapping(value = "/v1", method = POST)`
/// states more than this vocabulary can carry, and a partial reading of it
/// would be a fact nobody wrote.
fn annotations_of(node: Node, text: &str) -> Vec<(String, Option<String>)> {
    let Some(modifiers) = node
        .child_by_field_name("modifiers")
        .or_else(|| grammar::find(node, "modifiers"))
    else {
        return Vec::new();
    };
    let mut children = Vec::new();
    grammar::children(modifiers, &mut children);
    let mut out = Vec::new();
    for child in children {
        match child.kind() {
            "marker_annotation" => {
                if let Some(name) = grammar::field(child, text, "name") {
                    out.push((name, None));
                }
            }
            "annotation" => {
                let Some(name) = grammar::field(child, text, "name") else {
                    continue;
                };
                let argument = child
                    .child_by_field_name("arguments")
                    .and_then(|list| annotation_path(list, text));
                out.push((name, argument));
            }
            _ => {}
        }
    }
    out
}

/// The path an annotation argument list states, in any of the forms Spring
/// accepts.
///
/// Reading only a lone literal meant `@GetMapping(value = "/x")`,
/// `@GetMapping(path = "/x")` and `@GetMapping({"/x"})` all lost their path and
/// fell back to the class prefix, which INVENTED a route at the prefix itself
/// and lost the real one. All three are as explicit as the bare form.
fn annotation_path(list: Node, text: &str) -> Option<String> {
    let mut args = Vec::new();
    grammar::children(list, &mut args);
    for argument in &args {
        match argument.kind() {
            // `value = "/x"` / `path = "/x"`; any other named argument
            // (`produces`, `method`, `consumes`) says nothing about the path.
            "assignment_expression" => {
                let key = grammar::field(*argument, text, "left").unwrap_or_default();
                if key != "value" && key != "path" {
                    continue;
                }
                if let Some(found) = first_string(*argument, text) {
                    return Some(found);
                }
            }
            // A bare literal, or `{"/a", "/b"}`. Only the first element of an
            // array is taken: the others are real paths too, but this
            // vocabulary carries one path per operation and inventing a
            // second entry for the same handler would misreport the surface.
            _ => {
                if let Some(found) = first_string(*argument, text) {
                    return Some(found);
                }
                // A non-string argument is still an argument: `@Min(1)` and
                // `@Max(5)` carry numbers, and their callers read this same
                // field. Only a NAMED argument is skipped above, because a
                // `produces=` says nothing about the path.
                let raw = grammar::text(*argument, text).trim();
                if !raw.is_empty() {
                    return Some(raw.to_string());
                }
            }
        }
    }
    None
}

/// The first string literal anywhere in an argument.
fn first_string(node: Node, text: &str) -> Option<String> {
    let literal = grammar::find(node, "string_literal")?;
    Some(grammar::unquote(grammar::text(literal, text)).to_string())
}

/// `List<BlockRequest>` -> `BlockRequest`, `com.x.T` -> `T`.
fn bare_type(raw: &str) -> String {
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

fn is_builtin(name: &str) -> bool {
    matches!(
        name,
        "String"
            | "int"
            | "Integer"
            | "long"
            | "Long"
            | "double"
            | "Double"
            | "float"
            | "Float"
            | "boolean"
            | "Boolean"
            | "short"
            | "Short"
            | "byte"
            | "Byte"
            | "char"
            | "Character"
            | "BigDecimal"
            | "BigInteger"
            | "Object"
            | "UUID"
            | "Instant"
            | "LocalDate"
            | "LocalDateTime"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_source(case: &str, files: &[(&str, &str)]) -> SourceRead {
        let root =
            std::env::temp_dir().join(format!("reproit-javaast-{}-{case}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("root");
        for (name, body) in files {
            std::fs::write(root.join(name), body).expect("write");
        }
        let source = read(&root);
        let _ = std::fs::remove_dir_all(&root);
        source
    }

    const CONTROLLER: &str = r#"@RestController
@RequestMapping("/v1")
public class BlockController {
    @PostMapping("/blocks")
    public ResponseEntity<Void> createBlock(@Valid @RequestBody BlockRequest body) {
        return null;
    }
}
"#;

    #[test]
    fn a_class_prefix_composes_with_the_method_mapping() {
        let source = read_source("prefix", &[("BlockController.java", CONTROLLER)]);
        assert_eq!(
            source.routes,
            vec![("/v1/blocks".to_string(), "post", Some("createBlock".into()))]
        );
    }

    #[test]
    fn a_request_body_type_resolves_its_bean_validation_constraints() {
        let source = read_source(
            "beanvalidation",
            &[
                ("BlockController.java", CONTROLLER),
                (
                    "BlockRequest.java",
                    "public class BlockRequest {\n    @NotNull\n    private String blockedId;\n\
                     \x20   @Min(-1)\n    @Max(1)\n    private int rating;\n}\n",
                ),
            ],
        );
        let fields = source.bodies.get("createBlock").expect("resolved");
        assert!(fields["blockedId"].required);
        assert!(
            !fields["rating"].required,
            "a primitive is not a constraint"
        );
        assert_eq!(fields["rating"].range, Some((Some(-1.0), Some(1.0))));
    }

    #[test]
    fn a_json_property_rename_is_the_wire_name() {
        let source = read_source(
            "rename",
            &[
                ("BlockController.java", CONTROLLER),
                (
                    "BlockRequest.java",
                    "public class BlockRequest {\n    @JsonProperty(\"blocked_type\")\n\
                     \x20   private String blockedType;\n}\n",
                ),
            ],
        );
        let fields = source.bodies.get("createBlock").expect("resolved");
        assert!(
            fields.contains_key("blocked_type"),
            "the wire name must win: {fields:?}"
        );
        assert!(!fields.contains_key("blockedType"));
    }

    #[test]
    fn an_enum_typed_field_is_a_closed_value_set() {
        let source = read_source(
            "enums",
            &[
                ("BlockController.java", CONTROLLER),
                (
                    "BlockRequest.java",
                    "public class BlockRequest {\n    private BlockedType blockedType;\n}\n",
                ),
                (
                    "BlockedType.java",
                    "public enum BlockedType { USER, SPONSOR }\n",
                ),
            ],
        );
        let fields = source.bodies.get("createBlock").expect("resolved");
        assert_eq!(
            fields["blockedType"].allowed.as_deref(),
            Some(["USER".to_string(), "SPONSOR".to_string()].as_slice())
        );
    }

    #[test]
    fn a_bare_mapping_maps_the_prefix_itself() {
        let source = read_source(
            "bare",
            &[(
                "C.java",
                "@RequestMapping(\"/v1/things\")\npublic class C {\n    @GetMapping\n\
                 \x20   public String list() { return \"\"; }\n}\n",
            )],
        );
        assert_eq!(
            source.routes,
            vec![("/v1/things".to_string(), "get", Some("list".into()))]
        );
    }

    #[test]
    fn a_dto_in_the_controllers_file_does_not_inherit_its_prefix() {
        // The line reader took the file's first @RequestMapping as the prefix
        // for everything after it, so a second class in the same file got a
        // path its own class never declared.
        let source = read_source(
            "twoclasses",
            &[(
                "Both.java",
                "@RequestMapping(\"/v1\")\npublic class A {\n    @GetMapping(\"/a\")\n\
                 \x20   public String a() { return \"\"; }\n}\n\
                 public class B {\n    @GetMapping(\"/b\")\n    public String b() { return \"\"; }\n}\n",
            )],
        );
        let paths: Vec<&String> = source.routes.iter().map(|(path, _, _)| path).collect();
        assert!(paths.contains(&&"/v1/a".to_string()), "{paths:?}");
        assert!(
            paths.contains(&&"/b".to_string()),
            "the second class has no prefix: {paths:?}"
        );
    }

    #[test]
    fn a_record_states_its_components_as_fields() {
        let source = read_source(
            "record",
            &[
                ("BlockController.java", CONTROLLER),
                (
                    "BlockRequest.java",
                    "public record BlockRequest(@NotNull String blockedId, @Min(1) @Max(5) int rating) {}\n",
                ),
            ],
        );
        let fields = source.bodies.get("createBlock").expect("resolved");
        assert!(fields["blockedId"].required);
        assert_eq!(fields["rating"].range, Some((Some(1.0), Some(5.0))));
    }

    #[test]
    fn two_types_of_the_same_name_resolve_to_neither() {
        let source = read_source(
            "ambiguous",
            &[
                ("BlockController.java", CONTROLLER),
                (
                    "a.java",
                    "public class BlockRequest {\n    @NotNull\n    private String a;\n}\n",
                ),
                (
                    "b.java",
                    "public class BlockRequest {\n    @NotNull\n    private String b;\n}\n",
                ),
            ],
        );
        assert!(
            !source.bodies.contains_key("createBlock"),
            "an ambiguous type must abstain: {:?}",
            source.bodies
        );
    }

    #[test]
    fn a_file_that_does_not_parse_is_counted() {
        let source = read_source(
            "broken",
            &[
                ("Ok.java", "class Ok { int x; }\n"),
                ("Bad.java", "class Bad { void f( {\n"),
            ],
        );
        assert_eq!(source.files_parsed, 1);
        assert_eq!(source.files_unreadable, 1);
    }
}
