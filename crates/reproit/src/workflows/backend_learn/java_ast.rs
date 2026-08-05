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
use super::field_facts::{apply_rename_all, bare_type, drop_ambiguous, record, FieldFact};
use super::grammar::{self, SourceRead, MAX_FIELDS};
use super::java_types;
use super::response_facts::{ResponseFact, Serializers, WireField};
use super::route_path::join_segments as join;
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
const MAX_MAPPING_VALUES: usize = 64;

/// Everything the class walk collects, one bundle so a visitor names one
/// argument instead of seven.
#[derive(Default)]
struct Collected {
    /// type name -> its Bean Validation constrained fields (request side).
    shapes: BTreeMap<String, BTreeMap<String, FieldFact>>,
    ambiguous: BTreeSet<String>,
    /// type name -> its Jackson wire fields (response side).
    wire: Serializers,
    wire_ambiguous: BTreeSet<String>,
    /// enum name -> its constants, so an enum-typed field is a closed set.
    enums: BTreeMap<String, Vec<String>>,
    /// handler method -> the `@RequestBody` type it accepts.
    handler_body: BTreeMap<String, String>,
    /// handler method -> its `@RequestParam` query parameters.
    queries: BTreeMap<String, BTreeMap<String, FieldFact>>,
    /// handler method -> the response statuses and bodies its code states.
    responses: BTreeMap<String, ResponseFact>,
    /// (type, wire field) -> the enum type it is declared as, resolved once
    /// every file has been read: the constants live in another declaration.
    pending: Vec<(String, String, String)>,
    found: Vec<(String, &'static str, Option<String>)>,
}

pub(super) fn read(root: &Path) -> SourceRead {
    let mut source = SourceRead::default();
    let mut collected = Collected::default();

    grammar::read_files(
        root,
        Family::Spring,
        tree_sitter_java::LANGUAGE.into(),
        &mut source,
        |root_node, text, _path| {
            grammar::walk(root_node, &mut |node| match node.kind() {
                "class_declaration" | "record_declaration" => {
                    collect_class(node, text, &mut collected);
                }
                "enum_declaration" => collect_enum(node, text, &mut collected.enums),
                _ => {}
            });
        },
    );
    drop_ambiguous(&mut collected.shapes, &collected.ambiguous);
    drop_ambiguous(&mut collected.wire, &collected.wire_ambiguous);
    // An enum-typed field's accepted set is the enum's constants. A type that
    // is not a known enum stays open rather than becoming an empty set: not
    // finding the declaration is not evidence that the field accepts nothing.
    for (owner, field, enum_type) in collected.pending {
        let Some(values) = collected
            .enums
            .get(&enum_type)
            .filter(|values| values.len() > 1)
        else {
            continue;
        };
        if let Some(fact) = collected
            .shapes
            .get_mut(&owner)
            .and_then(|f| f.get_mut(&field))
        {
            fact.allowed = Some(values.clone());
            fact.evidence = Some("an enum-typed field".to_string());
        }
    }

    for (path, method, handler) in collected.found {
        source.routes.push((path, method, handler.clone()));
        if let Some(handler) = handler {
            if let Some(fields) = collected
                .handler_body
                .get(&handler)
                .and_then(|name| collected.shapes.get(name))
                .cloned()
            {
                source.bodies.insert(handler.clone(), fields);
            }
            if let Some(fields) = collected.queries.get(&handler) {
                source.queries.insert(handler.clone(), fields.clone());
            }
            if let Some(fact) = collected.responses.get(&handler) {
                source.responses.insert(handler, fact.clone());
            }
        }
    }
    source.serializers = collected.wire;
    source
}

/// One class: its mapping prefix, its handler methods, and its own fields.
///
/// The prefix applies to this class body only. A file holding a controller and
/// a DTO used to leak the controller's `@RequestMapping` onto the DTO.
///
/// The wire (response) side abstains from a class it cannot read whole: a
/// superclass promotes fields declared elsewhere, and a Jackson annotation on
/// a METHOD (`@JsonIgnore` on a getter, `@JsonValue`) reshapes the output past
/// what the fields state. A partial wire shape would claim fields the type
/// does not serialize, so those classes state nothing. Same rule as serde
/// `flatten` and Go embedding.
fn collect_class(node: Node, text: &str, collected: &mut Collected) {
    let Some(name) = grammar::field(node, text, "name") else {
        return;
    };
    let annotations = annotations_of(node, text);
    let prefix = annotations
        .iter()
        .find(|(name, _)| name == "RequestMapping")
        .and_then(|(_, argument)| argument.clone())
        .unwrap_or_default();
    let mut context = java_types::WireContext {
        // A class-level `@JsonInclude` makes every field's omission conditional.
        conditional: annotations.iter().any(|(name, _)| name == "JsonInclude"),
        // Lombok generates the getters the fields alone do not state.
        exposed: annotations
            .iter()
            .any(|(name, _)| matches!(name.as_str(), "Data" | "Getter" | "Value")),
        getters: BTreeSet::new(),
    };
    let mut abstain_wire = node.child_by_field_name("superclass").is_some()
        || annotations
            .iter()
            .any(|(name, _)| name == "JsonSerialize" || name == "JsonTypeInfo");
    let Some(body) = node.child_by_field_name("body") else {
        return;
    };
    let mut members = Vec::new();
    grammar::children(body, &mut members);
    // Pre-pass: the getters this class declares, and the Jackson method
    // annotations that make its output unreadable from the fields.
    for member in &members {
        if member.kind() != "method_declaration" {
            continue;
        }
        if let Some(method) = grammar::field(*member, text, "name") {
            context.getters.insert(method);
        }
        if annotations_of(*member, text).iter().any(|(name, _)| {
            matches!(
                name.as_str(),
                "JsonIgnore" | "JsonProperty" | "JsonValue" | "JsonAnyGetter"
            )
        }) {
            abstain_wire = true;
        }
    }
    let mut fields = BTreeMap::new();
    let mut wire = BTreeMap::new();
    for member in members {
        match member.kind() {
            "method_declaration" => {
                collect_method(member, text, &prefix, collected);
            }
            "field_declaration" => collect_field(
                member,
                text,
                &name,
                &mut fields,
                &mut wire,
                &context,
                collected,
            ),
            _ => {}
        }
    }
    // A record states its components as parameters, not as fields; every
    // component serializes, so the getter gate does not apply.
    if let Some(parameters) = node.child_by_field_name("parameters") {
        context.exposed = true;
        let mut components = Vec::new();
        grammar::children(parameters, &mut components);
        for component in components {
            collect_field(
                component,
                text,
                &name,
                &mut fields,
                &mut wire,
                &context,
                collected,
            );
        }
    }
    if !fields.is_empty() {
        record(
            &mut collected.shapes,
            &mut collected.ambiguous,
            name.clone(),
            fields,
        );
    }
    if !wire.is_empty() && !abstain_wire {
        record(
            &mut collected.wire,
            &mut collected.wire_ambiguous,
            name,
            wire,
        );
    }
}

/// `@PostMapping("/blocks") ResponseEntity<Void> createBlock(@RequestBody T b)`
fn collect_method(node: Node, text: &str, prefix: &str, collected: &mut Collected) -> Option<()> {
    let handler = grammar::field(node, text, "name")?;
    let annotations = annotations_of(node, text);
    let mapping = annotations.iter().find_map(|(name, _)| {
        MAPPINGS
            .into_iter()
            .find(|(mapping, _)| mapping == name)
            .map(|(_, verb)| (name.as_str(), verb))
    });
    // `@RequestMapping(method = RequestMethod.PUT)` states its verb in an
    // argument rather than in the annotation name. Without a method it serves
    // every verb, and claiming one would be an invention, so GET is taken as
    // the one a draft can exercise without mutating anything.
    let (verbs, paths) = match mapping {
        Some((name, verb)) => (vec![verb], mapping_paths(node, text, name)),
        None => {
            annotations
                .iter()
                .find(|(name, _)| name == "RequestMapping")?;
            let verbs = request_verbs(node, text);
            (
                if verbs.is_empty() { vec!["get"] } else { verbs },
                mapping_paths(node, text, "RequestMapping"),
            )
        }
    };
    // Spring templates are as often relative as absolute (`@GetMapping("owners")`).
    // Treating a relative one as absent collapsed it onto the class prefix,
    // inventing a route at the prefix and losing the real path.
    if let Some(parameters) = node.child_by_field_name("parameters") {
        let mut params = Vec::new();
        grammar::children(parameters, &mut params);
        for parameter in params {
            let takes_body = annotations_of(parameter, text)
                .iter()
                .any(|(name, _)| name == "RequestBody");
            if takes_body {
                if let Some(ty) = grammar::field(parameter, text, "type") {
                    collected
                        .handler_body
                        .insert(handler.clone(), bare_type(&ty));
                }
            }
            if let Some((name, fact)) = request_param(parameter, text) {
                let fields = collected.queries.entry(handler.clone()).or_default();
                if fields.len() < MAX_FIELDS {
                    fields.insert(name, fact);
                }
            }
        }
    }
    // What this handler states it returns. `@ResponseStatus` names its code in
    // an argument the lone-literal annotation reader cannot carry (an enum
    // constant), so the argument list is read here by its own rule.
    let declared = annotation_arguments(node, text, "ResponseStatus")
        .and_then(|arguments| java_types::status_of(grammar::text(arguments, text)));
    if let Some(fact) = java_types::response_of(node, text, declared) {
        collected.responses.insert(handler.clone(), fact);
    }
    if paths.is_empty() {
        for verb in verbs {
            collected
                .found
                .push((join(prefix, ""), verb, Some(handler.clone())));
        }
    } else {
        for path in paths {
            for verb in &verbs {
                collected
                    .found
                    .push((join(prefix, &path), *verb, Some(handler.clone())));
            }
        }
    }
    Some(())
}

/// The verbs a `@RequestMapping(method = {...})` names, in source order.
fn request_verbs(node: Node, text: &str) -> Vec<&'static str> {
    let Some(arguments) = annotation_arguments(node, text, "RequestMapping") else {
        return Vec::new();
    };
    let mut methods = None;
    grammar::walk(arguments, &mut |argument| {
        if methods.is_none()
            && is_annotation_pair(argument)
            && annotation_pair_key(argument, text).as_deref() == Some("method")
        {
            methods = Some(grammar::text(argument, text).to_string());
        }
    });
    let Some(methods) = methods else {
        return Vec::new();
    };
    let mut remaining = methods.as_str();
    let mut verbs = Vec::new();
    while verbs.len() < MAX_MAPPING_VALUES {
        let Some(offset) = remaining.find("RequestMethod.") else {
            break;
        };
        remaining = &remaining[offset + "RequestMethod.".len()..];
        let name: String = remaining
            .chars()
            .take_while(|character| character.is_ascii_alphabetic())
            .collect();
        let lower = name.to_ascii_lowercase();
        if let Some(verb) = MAPPINGS
            .into_iter()
            .map(|(_, known)| known)
            .find(|known| *known == lower && !verbs.contains(known))
        {
            verbs.push(verb);
        }
    }
    verbs
}

/// A `@RequestParam` parameter: its wire name and whether Spring demands it.
///
/// The wire name is the annotation's literal (`@RequestParam("q")`, or the
/// `value =` / `name =` pair) and otherwise the parameter's own name, which is
/// exactly Spring's resolution order. Spring demands the parameter unless the
/// annotation states `required = false` or supplies a `defaultValue`, so a
/// bare `@RequestParam String q` is a demand the source spells out.
fn request_param(parameter: Node, text: &str) -> Option<(String, FieldFact)> {
    annotations_of(parameter, text)
        .iter()
        .find(|(name, _)| name == "RequestParam")?;
    let mut wire = None;
    let mut required = true;
    if let Some(arguments) = annotation_arguments(parameter, text, "RequestParam") {
        let mut args = Vec::new();
        grammar::children(arguments, &mut args);
        for argument in args {
            if !is_annotation_pair(argument) {
                if wire.is_none() {
                    wire = first_string(argument, text);
                }
                continue;
            }
            let key = annotation_pair_key(argument, text).unwrap_or_default();
            let value = annotation_pair_value(argument);
            match key.as_str() {
                "value" | "name" => {
                    wire = value.and_then(|value| first_string(value, text)).or(wire);
                }
                "required" => {
                    if value.is_some_and(|value| grammar::text(value, text).trim() == "false") {
                        required = false;
                    }
                }
                "defaultValue" => required = false,
                _ => {}
            }
        }
    }
    let name = wire.or_else(|| grammar::field(parameter, text, "name"))?;
    let fact = FieldFact {
        required,
        evidence: Some("a @RequestParam annotation".to_string()),
        ..FieldFact::default()
    };
    Some((name, fact))
}

/// A field or record component: what its annotations constrain on the
/// request side, and what Jackson writes for it on the response side.
fn collect_field(
    node: Node,
    text: &str,
    owner: &str,
    fields: &mut BTreeMap<String, FieldFact>,
    wire_fields: &mut BTreeMap<String, WireField>,
    context: &java_types::WireContext,
    collected: &mut Collected,
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
        collected
            .pending
            .push((owner.to_string(), wire.clone(), bare));
    }
    // The response side: what Jackson writes for this field. A static or
    // transient field never serializes, `@JsonIgnore` says so explicitly, a
    // field with no getter is unreachable, and a field-level `@JsonInclude`
    // makes its omission conditional.
    let ignored = annotations.iter().any(|(name, _)| name == "JsonIgnore")
        || java_types::has_modifier(node, text, &["static", "transient"])
        || !context.serializes(node, text, &name);
    if !ignored && wire_fields.len() < MAX_FIELDS {
        let field_conditional =
            context.conditional || annotations.iter().any(|(name, _)| name == "JsonInclude");
        wire_fields.insert(wire.clone(), java_types::wire_field(&ty, field_conditional));
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
            "assignment_expression" | "element_value_pair" => {
                let key = annotation_pair_key(*argument, text).unwrap_or_default();
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
                // field. But an IDENTIFIER is a constant this cannot resolve,
                // and returning its text made `@GetMapping(SOME_CONST)` into
                // the literal route `/p/SOME_CONST`.
                let raw = grammar::text(*argument, text).trim();
                let resolvable = !raw.is_empty()
                    && raw
                        .chars()
                        .all(|c| c.is_ascii_digit() || c == '-' || c == '.');
                if resolvable {
                    return Some(raw.to_string());
                }
            }
        }
    }
    None
}

/// Every literal path on one route annotation, in source order.
///
/// Constants and expressions remain unreadable. Expanding only string
/// literals preserves the no-invented-path rule while retaining Spring's
/// explicit array form.
fn mapping_paths(node: Node, text: &str, annotation: &str) -> Vec<String> {
    let Some(arguments) = annotation_arguments(node, text, annotation) else {
        return Vec::new();
    };
    let mut args = Vec::new();
    grammar::children(arguments, &mut args);
    let mut paths = Vec::new();
    for argument in args {
        if paths.len() >= MAX_MAPPING_VALUES {
            break;
        }
        let value = if is_annotation_pair(argument) {
            let key = annotation_pair_key(argument, text).unwrap_or_default();
            if key != "value" && key != "path" {
                continue;
            }
            annotation_pair_value(argument).unwrap_or(argument)
        } else {
            argument
        };
        grammar::walk(value, &mut |inner| {
            if paths.len() >= MAX_MAPPING_VALUES || inner.kind() != "string_literal" {
                return;
            }
            paths.push(grammar::unquote(grammar::text(inner, text)).to_string());
        });
    }
    paths
}

fn is_annotation_pair(node: Node) -> bool {
    matches!(node.kind(), "assignment_expression" | "element_value_pair")
}

fn annotation_pair_key(node: Node, text: &str) -> Option<String> {
    grammar::field(node, text, "left").or_else(|| grammar::field(node, text, "key"))
}

fn annotation_pair_value(node: Node) -> Option<Node> {
    node.child_by_field_name("right")
        .or_else(|| node.child_by_field_name("value"))
}

fn annotation_arguments<'tree>(node: Node<'tree>, text: &str, wanted: &str) -> Option<Node<'tree>> {
    let modifiers = node
        .child_by_field_name("modifiers")
        .or_else(|| grammar::find(node, "modifiers"))?;
    let mut children = Vec::new();
    grammar::children(modifiers, &mut children);
    children.into_iter().find_map(|child| {
        if child.kind() != "annotation"
            || grammar::field(child, text, "name").as_deref() != Some(wanted)
        {
            return None;
        }
        child.child_by_field_name("arguments")
    })
}

/// The first string literal anywhere in an argument.
fn first_string(node: Node, text: &str) -> Option<String> {
    let literal = grammar::find(node, "string_literal")?;
    Some(grammar::unquote(grammar::text(literal, text)).to_string())
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
    fn a_request_param_states_its_name_and_springs_demand() {
        let source = read_source(
            "requestparam",
            &[(
                "SearchController.java",
                "@RestController\n@RequestMapping(\"/search\")\npublic class SearchController {\n\
                 \x20   @GetMapping\n\
                 \x20   public String search(@RequestParam String q,\n\
                 \x20           @RequestParam(\"page_size\") int size,\n\
                 \x20           @RequestParam(value = \"sort\", required = false) String sort,\n\
                 \x20           @RequestParam(defaultValue = \"10\") int limit) {\n\
                 \x20       return \"\";\n    }\n}\n",
            )],
        );
        let fields = source.queries.get("search").expect("stated");
        assert!(fields["q"].required, "a bare @RequestParam is a demand");
        assert!(
            fields.contains_key("page_size") && !fields.contains_key("size"),
            "the annotation literal is the wire name: {:?}",
            fields.keys()
        );
        assert!(
            !fields["sort"].required,
            "required = false lifts the demand"
        );
        assert!(!fields["limit"].required, "a defaultValue lifts the demand");
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
    fn every_spring_annotation_form_keeps_its_path() {
        // The named-argument and array forms lost their path and fell back to
        // the class prefix, which invented a route AT the prefix.
        let source = read_source(
            "forms",
            &[(
                "A.java",
                "@RestController\n@RequestMapping(\"/p\")\nclass A {\n\
                 \x20 @GetMapping(\"/plain\") String s1() { return \"\"; }\n\
                 \x20 @GetMapping({\"/arr\"}) String s2() { return \"\"; }\n\
                 \x20 @GetMapping(value = \"/namedval\") String s3() { return \"\"; }\n\
                 \x20 @GetMapping(path = \"/namedpath\") String s4() { return \"\"; }\n\
                 \x20 @RequestMapping(value=\"/rm\", method=RequestMethod.PUT) String s6() { return \"\"; }\n\
                 \x20 @GetMapping(\"relative\") String s8() { return \"\"; }\n}\n",
            )],
        );
        let paths: Vec<&String> = source.routes.iter().map(|(path, _, _)| path).collect();
        for expected in [
            "/p/plain",
            "/p/arr",
            "/p/namedval",
            "/p/namedpath",
            "/p/rm",
            "/p/relative",
        ] {
            assert!(
                paths.contains(&&expected.to_string()),
                "{expected}: {paths:?}"
            );
        }
        assert!(
            !paths.contains(&&"/p".to_string()),
            "nothing serves the bare class prefix: {paths:?}"
        );
        let put = source
            .routes
            .iter()
            .find(|(path, _, _)| path == "/p/rm")
            .map(|(_, method, _)| *method);
        assert_eq!(put, Some("put"), "the verb comes from method=");
    }

    #[test]
    fn spring_mapping_arrays_contribute_every_literal_path_and_method() {
        let source = read_source(
            "mapping-arrays",
            &[(
                "A.java",
                "@RestController\n@RequestMapping(\"/api\")\nclass A {\n\
                 \x20 @PostMapping(value = {\"/m1\", \"/m2\"}) String create() { return \"\"; }\n\
                 \x20 @RequestMapping(value = \"/q\", method = {\n\
                 \x20     RequestMethod.GET, RequestMethod.POST\n\
                 \x20 }) String query() { return \"\"; }\n}\n",
            )],
        );
        assert_eq!(
            source.routes,
            vec![
                ("/api/m1".to_string(), "post", Some("create".to_string())),
                ("/api/m2".to_string(), "post", Some("create".to_string())),
                ("/api/q".to_string(), "get", Some("query".to_string())),
                ("/api/q".to_string(), "post", Some("query".to_string())),
            ]
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
