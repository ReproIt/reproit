//! ASP.NET Core route and validation extraction over the C# grammar.
//!
//! Two idioms, both common and both in the same file often enough that the
//! reader has to handle them together:
//!
//! - minimal APIs, where the route IS a call: `app.MapGet("/health", handler)`
//! - controllers, where the route is split between a class attribute
//!   (`[Route("api/users")]`) and a method attribute (`[HttpGet("{id}")]`)
//!
//! The controller half is the same shape as Spring: an attribute attached to
//! the declaration below it, and a class-level prefix that applies to THAT
//! class body only.

use super::dotnet_types;
use super::extract::Family;
use super::field_facts::{bare_type, drop_ambiguous, record, FieldFact};
use super::grammar::{self, SourceRead, MAX_FIELDS};
use super::response_facts::{ResponseFact, Serializers, WireField};
use super::route_path::join_segments as join;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use tree_sitter::Node;

/// `MapGet` and `[HttpGet]` both name the verb in their suffix.
const METHODS: [&str; 7] = ["get", "post", "put", "patch", "delete", "head", "options"];
const MAX_GROUP_DEPTH: usize = 8;
const OPAQUE: &str = "\u{1}opaque";
type Groups = BTreeMap<String, (String, String)>;

/// Everything the class walk collects, one bundle so a visitor names one
/// argument instead of six.
#[derive(Default)]
struct Collected {
    /// type name -> its data-annotation constrained properties (request side).
    shapes: BTreeMap<String, BTreeMap<String, FieldFact>>,
    ambiguous: BTreeSet<String>,
    /// type name -> its serialized wire properties (response side).
    wire: Serializers,
    wire_ambiguous: BTreeSet<String>,
    /// action method -> the `[FromBody]` type it accepts.
    handler_body: BTreeMap<String, String>,
    /// action method -> the response statuses and bodies its code states.
    responses: BTreeMap<String, ResponseFact>,
    found: Vec<(String, &'static str, Option<String>)>,
}

pub(super) fn read(root: &Path) -> SourceRead {
    let mut source = SourceRead::default();
    let mut collected = Collected::default();

    grammar::read_files(
        root,
        Family::DotNet,
        tree_sitter_c_sharp::LANGUAGE.into(),
        &mut source,
        |root_node, text, _path| {
            // `var g = app.MapGroup("/api")` binds a prefix to a local name,
            // which is a per-file binding. Without it every minimal-API path in
            // a grouped service was reported one prefix short.
            let mut groups = Groups::new();
            grammar::walk(root_node, &mut |node| {
                if node.kind() == "variable_declarator" {
                    collect_group(node, text, &mut groups);
                }
            });
            grammar::walk(root_node, &mut |node| match node.kind() {
                "invocation_expression" => minimal_api(node, text, &groups, &mut collected.found),
                "class_declaration" => collect_class(node, text, &mut collected),
                "record_declaration" => collect_record(node, text, &mut collected),
                _ => {}
            });
        },
    );
    drop_ambiguous(&mut collected.shapes, &collected.ambiguous);
    drop_ambiguous(&mut collected.wire, &collected.wire_ambiguous);

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
            if let Some(fact) = collected.responses.get(&handler) {
                source.responses.insert(handler, fact.clone());
            }
        }
    }
    source.serializers = collected.wire;
    source
}

/// `app.MapGet("/health", handler)` and `app.MapPost("/items", ...)`.
/// `var api = app.MapGroup("api/orders");` -> `api` carries that prefix.
fn collect_group(node: Node, text: &str, groups: &mut Groups) {
    let Some(name) = grammar::field(node, text, "name") else {
        return;
    };
    let Some(prefix) = map_group_prefix(node, text) else {
        return;
    };
    let parent = map_group_receiver(node, text).unwrap_or_default();
    groups.insert(name, (parent, prefix));
}

/// The prefix a `MapGroup("...")` anywhere in this expression establishes.
fn map_group_prefix(node: Node, text: &str) -> Option<String> {
    let mut found = Vec::new();
    grammar::walk(node, &mut |inner| {
        if inner.kind() != "invocation_expression" {
            return;
        }
        let Some(callee) = inner.child_by_field_name("function") else {
            return;
        };
        if grammar::field(callee, text, "name").as_deref() != Some("MapGroup") {
            return;
        }
        let Some(arguments) = inner.child_by_field_name("arguments") else {
            return;
        };
        let mut args = Vec::new();
        grammar::children(arguments, &mut args);
        let prefix = args
            .first()
            .and_then(|arg| grammar::find(*arg, "string_literal"))
            .map(|literal| grammar::unquote(grammar::text(literal, text)).to_string())
            .unwrap_or_else(|| OPAQUE.to_string());
        found.push(prefix);
    });
    let mut prefixes = found.into_iter().rev();
    let first = prefixes.next()?;
    Some(prefixes.fold(first, |outer, own| join(&outer, &own)))
}

/// Receiver of the outermost `MapGroup` call in an assignment expression.
fn map_group_receiver(node: Node, text: &str) -> Option<String> {
    let mut receiver = None;
    grammar::walk(node, &mut |inner| {
        if receiver.is_some() || inner.kind() != "invocation_expression" {
            return;
        }
        let Some(callee) = inner.child_by_field_name("function") else {
            return;
        };
        if grammar::field(callee, text, "name").as_deref() == Some("MapGroup") {
            receiver = grammar::field(callee, text, "expression");
        }
    });
    receiver
}

fn resolve_group_prefix(groups: &Groups, group: &str) -> Option<String> {
    let (mut parent, own) = groups.get(group)?.clone();
    let mut prefix = own;
    let mut seen = BTreeSet::from([group.to_string()]);
    for _ in 0..MAX_GROUP_DEPTH {
        if !seen.insert(parent.clone()) {
            return None;
        }
        let Some((outer, own)) = groups.get(&parent) else {
            break;
        };
        prefix = join(own, &prefix);
        parent = outer.clone();
    }
    if groups.contains_key(&parent) {
        return None;
    }
    Some(prefix)
}

fn minimal_api(
    node: Node,
    text: &str,
    groups: &Groups,
    found: &mut Vec<(String, &'static str, Option<String>)>,
) {
    let Some(callee) = node.child_by_field_name("function") else {
        return;
    };
    if callee.kind() != "member_access_expression" {
        return;
    }
    let called = grammar::field(callee, text, "name").unwrap_or_default();
    let Some(verb) = called.strip_prefix("Map").map(str::to_ascii_lowercase) else {
        return;
    };
    let Some(method) = METHODS.into_iter().find(|known| *known == verb) else {
        return;
    };
    let Some(arguments) = node.child_by_field_name("arguments") else {
        return;
    };
    let mut args = Vec::new();
    grammar::children(arguments, &mut args);
    // ASP.NET route templates are commonly written WITHOUT a leading slash
    // (`app.MapGet("api/catalog-brands", ...)`), and requiring one dropped 8 of
    // 9 endpoints in a real service. Only a string LITERAL is a template: an
    // identifier is a constant this cannot resolve, and guessing at it would
    // invent a path.
    let Some(literal) = args
        .first()
        .and_then(|arg| grammar::find(*arg, "string_literal"))
    else {
        return;
    };
    let raw = grammar::unquote(grammar::text(literal, text)).to_string();
    if raw.contains('[') {
        return;
    }
    // The receiver is either a grouped local or an inline `MapGroup(..)` chain.
    let receiver = grammar::field(callee, text, "expression").unwrap_or_default();
    let outer = if groups.contains_key(&receiver) {
        let Some(prefix) = resolve_group_prefix(groups, &receiver) else {
            return;
        };
        prefix
    } else {
        callee
            .child_by_field_name("expression")
            .and_then(|inner| map_group_prefix(inner, text))
            .unwrap_or_default()
    };
    if outer.contains(OPAQUE) {
        return;
    }
    found.push((join(&outer, &raw), method, None));
}

/// `[action]` resolves to the method name, as ASP.NET substitutes it.
fn substitute_action(template: &str, handler: &str) -> String {
    template.replace("[action]", handler)
}

/// A controller class, or a DTO whose properties carry validation attributes.
///
/// The wire (response) side abstains from a class it cannot read whole: a
/// base list may name a class whose properties this type inherits, and a
/// custom `[JsonConverter]` rewrites the output past what the properties
/// state. A partial wire shape would claim fields the type does not
/// serialize, so those classes state nothing. Same rule as serde `flatten`
/// and Go embedding; it also keeps controllers themselves out, since every
/// controller extends `ControllerBase`.
fn collect_class(node: Node, text: &str, collected: &mut Collected) {
    let Some(name) = grammar::field(node, text, "name") else {
        return;
    };
    let attributes = attributes_of(node, text);
    // `[Route("api/users")]` prefixes this class body only. `[controller]` is
    // substituted by ASP.NET with the class name minus the Controller suffix.
    let prefix = attributes
        .iter()
        .find(|(attribute, _)| attribute == "Route")
        .and_then(|(_, argument)| argument.clone())
        .map(|route| substitute_tokens(&route, &name))
        .unwrap_or_default();
    let abstain_wire = has_base_list(node)
        || attributes
            .iter()
            .any(|(attribute, _)| attribute == "JsonConverter");
    let Some(body) = node.child_by_field_name("body") else {
        return;
    };
    let mut members = Vec::new();
    grammar::children(body, &mut members);
    let mut fields = BTreeMap::new();
    let mut wire = BTreeMap::new();
    for member in members {
        match member.kind() {
            "method_declaration" => collect_action(member, text, &prefix, collected),
            "property_declaration" | "field_declaration" => {
                collect_property(member, text, &mut fields, &mut wire)
            }
            _ => {}
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

/// A record DTO: its positional components serialize as properties named as
/// written, and a body may add ordinary properties on top.
fn collect_record(node: Node, text: &str, collected: &mut Collected) {
    let Some(name) = grammar::field(node, text, "name") else {
        return;
    };
    if has_base_list(node) {
        return;
    }
    let mut wire = BTreeMap::new();
    let mut children = Vec::new();
    grammar::children(node, &mut children);
    for child in children {
        if child.kind() != "parameter_list" {
            continue;
        }
        let mut components = Vec::new();
        grammar::children(child, &mut components);
        for component in components {
            if component.kind() != "parameter" || wire.len() >= MAX_FIELDS {
                continue;
            }
            let (Some(component_name), Some(ty)) = (
                grammar::field(component, text, "name"),
                grammar::field(component, text, "type"),
            ) else {
                continue;
            };
            let wire_name = attributes_of(component, text)
                .into_iter()
                .find(|(attribute, _)| attribute == "JsonPropertyName")
                .and_then(|(_, argument)| argument)
                .unwrap_or_else(|| dotnet_types::camel_case(&component_name));
            wire.insert(wire_name, dotnet_types::wire_field(&ty, false));
        }
    }
    if let Some(body) = node.child_by_field_name("body") {
        let mut members = Vec::new();
        grammar::children(body, &mut members);
        let mut fields = BTreeMap::new();
        for member in members {
            if matches!(member.kind(), "property_declaration" | "field_declaration") {
                collect_property(member, text, &mut fields, &mut wire);
            }
        }
    }
    if !wire.is_empty() {
        record(
            &mut collected.wire,
            &mut collected.wire_ambiguous,
            name,
            wire,
        );
    }
}

/// Whether a type declaration names bases (a class it inherits, or interfaces).
/// An interface adds no serialized state, but which kind a bare name is cannot
/// be resolved here, so any base list abstains.
fn has_base_list(node: Node) -> bool {
    let mut children = Vec::new();
    grammar::children(node, &mut children);
    children.iter().any(|child| child.kind() == "base_list")
}

/// `[HttpGet("{id}")] public IActionResult Get(int id)`.
fn collect_action(node: Node, text: &str, prefix: &str, collected: &mut Collected) {
    let Some(handler) = grammar::field(node, text, "name") else {
        return;
    };
    let attributes = attributes_of(node, text);
    let Some((method, argument)) = attributes.iter().find_map(|(attribute, argument)| {
        let verb = attribute.strip_prefix("Http")?.to_ascii_lowercase();
        METHODS
            .into_iter()
            .find(|known| *known == verb)
            .map(|method| (method, argument.clone()))
    }) else {
        return;
    };
    // The template can come from the verb attribute OR from a separate
    // `[Route("...")]` on the same method. Reading only the verb argument made
    // every action of a `[HttpGet] [Route("{id}")]` controller collapse onto
    // the class prefix, which invented a route nothing serves and lost the
    // real one.
    let template = argument.filter(|value| !value.is_empty()).or_else(|| {
        attributes
            .iter()
            .find(|(attribute, _)| attribute == "Route")
            .and_then(|(_, value)| value.clone())
            .filter(|value| !value.is_empty())
    });
    // No class template and no method template means the path comes from a
    // routing convention this cannot see. `/` is not it, and nothing serves
    // that: 14 actions of one real service collapsed onto it.
    if template.is_none() && prefix.is_empty() {
        return;
    }
    let path = match template {
        // A template starting with `/` REPLACES the controller template rather
        // than extending it: that is ASP.NET's rule, and composing it produced
        // a path the service does not serve.
        Some(suffix) if suffix.starts_with('/') => substitute_action(&suffix, &handler),
        Some(suffix) => join(prefix, &substitute_action(&suffix, &handler)),
        // A bare `[HttpGet]` serves the class route itself.
        None => join(prefix, ""),
    };
    // `[action]` in a class template resolves per action, so it can only be
    // substituted here.
    let path = substitute_action(&path, &handler);
    if path.contains('[') {
        // An unresolved token (`[area]`, a custom convention) means the real
        // path is not knowable from this declaration. Emitting it with the
        // brackets still in would be a route nobody serves.
        return;
    }
    // `[FromBody] ItemRequest body` names the type the action accepts.
    if let Some(parameters) = node.child_by_field_name("parameters") {
        let mut params = Vec::new();
        grammar::children(parameters, &mut params);
        for parameter in params {
            if !attributes_of(parameter, text)
                .iter()
                .any(|(attribute, _)| attribute == "FromBody")
            {
                continue;
            }
            let mut parts = Vec::new();
            grammar::children(parameter, &mut parts);
            if let Some(ty) = parts.iter().find(|part| {
                matches!(
                    part.kind(),
                    "identifier" | "generic_name" | "qualified_name"
                )
            }) {
                collected
                    .handler_body
                    .insert(handler.clone(), bare_type(grammar::text(*ty, text)));
            }
        }
    }
    // What this action states it returns, from its return type, the helper
    // calls in its body, and its `[ProducesResponseType]` declarations.
    if let Some(fact) = dotnet_types::response_of(node, text, &attributes) {
        collected.responses.insert(handler.clone(), fact);
    }
    collected.found.push((path, method, Some(handler)));
}

/// A DTO property: what its data-annotation attributes constrain on the
/// request side, and what the serializer writes for it on the response side.
fn collect_property(
    node: Node,
    text: &str,
    fields: &mut BTreeMap<String, FieldFact>,
    wire_fields: &mut BTreeMap<String, WireField>,
) {
    if fields.len() >= MAX_FIELDS {
        return;
    }
    let name = grammar::field(node, text, "name").or_else(|| {
        node.child_by_field_name("declarator")
            .and_then(|declarator| grammar::field(declarator, text, "name"))
    });
    let Some(name) = name else { return };
    let attributes = attributes_of(node, text);
    let mut fact = FieldFact::default();
    for (attribute, argument) in &attributes {
        match attribute.as_str() {
            "Required" => fact.required = true,
            "Range" => {
                let bounds: Vec<Option<f64>> = argument
                    .clone()
                    .unwrap_or_default()
                    .split(',')
                    .map(|part| grammar::number(part.trim()))
                    .collect();
                if let [low, high] = bounds[..] {
                    fact.range = Some((low, high));
                    fact.evidence = Some("a [Range] data annotation".to_string());
                }
            }
            _ => {}
        }
    }
    // `[JsonPropertyName("blocked_type")]` is the wire name.
    let rename = attributes
        .iter()
        .find(|(attribute, _)| attribute == "JsonPropertyName")
        .and_then(|(_, argument)| argument.clone());
    let wire = rename.clone().unwrap_or_else(|| name.clone());
    // The response side reads PROPERTIES only: System.Text.Json does not
    // serialize fields by default, so a field states nothing it can claim.
    // A bare `[JsonIgnore]` never serializes; one with a condition argument
    // serializes conditionally, so the field stays with `required` dropped.
    // A static property never serializes at all.
    let ignored = attributes
        .iter()
        .any(|(attribute, argument)| attribute == "JsonIgnore" && argument.is_none())
        || is_static(node, text);
    if node.kind() == "property_declaration" && !ignored && wire_fields.len() < MAX_FIELDS {
        let conditional = attributes
            .iter()
            .any(|(attribute, argument)| attribute == "JsonIgnore" && argument.is_some());
        if let Some(ty) = grammar::field(node, text, "type") {
            let written = rename.unwrap_or_else(|| dotnet_types::camel_case(&name));
            wire_fields.insert(written, dotnet_types::wire_field(&ty, conditional));
        }
    }
    fields.insert(wire, fact);
}

/// Whether a member declaration carries the `static` modifier.
fn is_static(node: Node, text: &str) -> bool {
    let mut children = Vec::new();
    grammar::children(node, &mut children);
    children
        .iter()
        .any(|child| child.kind() == "modifier" && grammar::text(*child, text) == "static")
}

/// The attributes on a declaration, each with its single literal argument.
///
/// Only a lone literal is read: a multi-argument attribute states more than
/// this vocabulary can carry, and a partial reading would be a fact nobody
/// wrote. `[Range(1, 5)]` is the exception, handled by its own caller.
fn attributes_of(node: Node, text: &str) -> Vec<(String, Option<String>)> {
    let mut out = Vec::new();
    let mut children = Vec::new();
    grammar::children(node, &mut children);
    for child in children {
        if child.kind() != "attribute_list" {
            continue;
        }
        let mut listed = Vec::new();
        grammar::children(child, &mut listed);
        for attribute in listed {
            if attribute.kind() != "attribute" {
                continue;
            }
            let Some(name) = grammar::field(attribute, text, "name") else {
                continue;
            };
            // The argument list is reached by KIND: the grammar does not give
            // it a field name on the attribute node, and asking for one
            // silently yielded no arguments, so every route collapsed to its
            // prefix and every `[Route(..)]` read as absent.
            let argument = grammar::find(attribute, "attribute_argument_list")
                .map(|list| {
                    let raw = grammar::text(list, text);
                    raw.trim_start_matches('(')
                        .trim_end_matches(')')
                        .split(',')
                        .map(|part| grammar::unquote(part.trim()).to_string())
                        .collect::<Vec<_>>()
                        .join(",")
                })
                .filter(|argument| !argument.is_empty());
            out.push((name, argument));
        }
    }
    out
}

/// ASP.NET substitutes `[controller]` with the class name minus its
/// `Controller` suffix, and `[action]` with the method name. Only the first is
/// resolvable from the class alone; a template carrying anything else is left
/// as written and will fail path normalization rather than be guessed at.
fn substitute_tokens(route: &str, class: &str) -> String {
    let controller = class.strip_suffix("Controller").unwrap_or(class);
    route.replace("[controller]", &controller.to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_source(case: &str, files: &[(&str, &str)]) -> SourceRead {
        let root =
            std::env::temp_dir().join(format!("reproit-csast-{}-{case}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("root");
        for (name, body) in files {
            std::fs::write(root.join(name), body).expect("write");
        }
        let source = read(&root);
        let _ = std::fs::remove_dir_all(&root);
        source
    }

    const CONTROLLER: &str = r#"
[ApiController]
[Route("api/[controller]")]
public class UsersController : ControllerBase
{
    [HttpGet]
    public IActionResult List() => Ok();

    [HttpGet("{id}")]
    public IActionResult Get(int id) => Ok();

    [HttpPost]
    public IActionResult Create([FromBody] ItemRequest body) => Ok();
}
"#;

    #[test]
    fn a_minimal_api_call_is_a_route() {
        let source = read_source(
            "minimal",
            &[(
                "Program.cs",
                "var app = WebApplication.Create();\n\
                 app.MapGet(\"/health\", () => \"ok\");\n\
                 app.MapPost(\"/items\", () => Results.Ok());\n\
                 app.Run();\n",
            )],
        );
        assert_eq!(
            source.routes,
            vec![
                ("/health".to_string(), "get", None),
                ("/items".to_string(), "post", None)
            ]
        );
    }

    #[test]
    fn a_controller_route_composes_with_its_class_attribute() {
        let source = read_source("controller", &[("UsersController.cs", CONTROLLER)]);
        let paths: Vec<&String> = source.routes.iter().map(|(path, _, _)| path).collect();
        // `[controller]` is substituted with the class name minus its suffix.
        assert!(paths.contains(&&"/api/users".to_string()), "{paths:?}");
        assert!(paths.contains(&&"/api/users/{id}".to_string()), "{paths:?}");
    }

    #[test]
    fn a_from_body_parameter_resolves_its_data_annotations() {
        let source = read_source(
            "annotations",
            &[
                ("UsersController.cs", CONTROLLER),
                (
                    "ItemRequest.cs",
                    "public class ItemRequest\n{\n    [Required]\n    public string Name { get; set; }\n\
                     \x20   [Range(1, 5)]\n    public int Size { get; set; }\n}\n",
                ),
            ],
        );
        let fields = source.bodies.get("Create").expect("resolved");
        assert!(fields["Name"].required);
        assert_eq!(fields["Size"].range, Some((Some(1.0), Some(5.0))));
        assert!(!fields["Size"].required, "a bare property is not required");
    }

    #[test]
    fn a_json_property_name_is_the_wire_name() {
        let source = read_source(
            "rename",
            &[
                ("UsersController.cs", CONTROLLER),
                (
                    "ItemRequest.cs",
                    "public class ItemRequest\n{\n    [JsonPropertyName(\"item_name\")]\n\
                     \x20   [Required]\n    public string Name { get; set; }\n}\n",
                ),
            ],
        );
        let fields = source.bodies.get("Create").expect("resolved");
        assert!(fields.contains_key("item_name"), "{:?}", fields.keys());
        assert!(!fields.contains_key("Name"));
    }

    #[test]
    fn a_method_level_route_composes_instead_of_collapsing() {
        // Every action of this controller carries its own `[Route]`. Reading
        // only the verb attribute collapsed them all onto the class prefix,
        // fabricating a bare route nothing serves and losing the real ones.
        let source = read_source(
            "method_route",
            &[(
                "C.cs",
                "[Route(\"integration-api/identity/users\")]\n\
                 public class IdentityUserIntegrationController : ControllerBase\n{\n\
                 \x20   [HttpGet]\n    [Route(\"{id}/role-names\")]\n\
                 \x20   public IActionResult R(Guid id) => Ok();\n\
                 \x20   [HttpGet]\n    [Route(\"count/roles\")]\n\
                 \x20   public IActionResult C() => Ok();\n}\n",
            )],
        );
        let paths: Vec<&String> = source.routes.iter().map(|(path, _, _)| path).collect();
        assert!(
            paths.contains(&&"/integration-api/identity/users/{id}/role-names".to_string()),
            "{paths:?}"
        );
        assert!(
            !paths.contains(&&"/integration-api/identity/users".to_string()),
            "nothing serves the bare class route: {paths:?}"
        );
    }

    #[test]
    fn map_group_composes_and_a_relative_template_is_a_path() {
        let source = read_source(
            "mapgroup",
            &[(
                "Program.cs",
                "var app = WebApplication.Create();\n\
                 app.MapGet(\"api/catalog-brands\", () => \"ok\");\n\
                 var api = app.MapGroup(\"api/orders\");\n\
                 api.MapGet(\"{orderId}\", () => \"ok\");\n",
            )],
        );
        let paths: Vec<&String> = source.routes.iter().map(|(path, _, _)| path).collect();
        assert!(
            paths.contains(&&"/api/catalog-brands".to_string()),
            "{paths:?}"
        );
        assert!(
            paths.contains(&&"/api/orders/{orderId}".to_string()),
            "{paths:?}"
        );
    }

    #[test]
    fn nested_map_group_variables_compose_through_their_parent() {
        let source = read_source(
            "nested-mapgroup",
            &[(
                "Program.cs",
                "var builder = WebApplication.CreateBuilder(args);\n\
                 var app = builder.Build();\n\
                 var api = app.MapGroup(\"api/v1\");\n\
                 var nested = api.MapGroup(\"/nested\");\n\
                 nested.MapPost(\"/deep\", () => Results.Ok());\n\
                 app.Run();\n",
            )],
        );
        assert_eq!(
            source.routes,
            vec![("/api/v1/nested/deep".to_string(), "post", None)]
        );
    }

    #[test]
    fn a_root_absolute_method_template_overrides_the_class() {
        let source = read_source(
            "absolute",
            &[(
                "C.cs",
                "[Route(\"api/v1/legacy\")]\npublic class LegacyController : ControllerBase\n{\n\
                 \x20   [HttpGet(\"/absolute/root\")] public IActionResult A() => Ok();\n}\n",
            )],
        );
        let paths: Vec<&String> = source.routes.iter().map(|(path, _, _)| path).collect();
        assert_eq!(paths, vec![&"/absolute/root".to_string()], "{paths:?}");
    }

    #[test]
    fn a_file_that_does_not_parse_is_counted() {
        let source = read_source(
            "broken",
            &[
                ("Ok.cs", "public class Ok { }\n"),
                ("Bad.cs", "public class Bad { void F( {\n"),
            ],
        );
        assert_eq!(source.files_parsed, 1);
        assert_eq!(source.files_unreadable, 1);
    }

    use super::super::response_facts::WireShape;

    #[test]
    fn action_result_helpers_state_status_and_body() {
        let source = read_source(
            "responses",
            &[
                (
                    "ItemsController.cs",
                    "[ApiController]\n[Route(\"api/items\")]\n\
                     public class ItemsController : ControllerBase\n{\n\
                     \x20   [HttpGet(\"{id}\")]\n\
                     \x20   public ActionResult<ItemDto> Get(int id)\n    {\n\
                     \x20       if (id == 0) { return NotFound(); }\n\
                     \x20       return Ok(item);\n    }\n\
                     \x20   [HttpPost]\n\
                     \x20   [ProducesResponseType(typeof(ItemDto), StatusCodes.Status201Created)]\n\
                     \x20   public IActionResult Create([FromBody] ItemDto body)\n    {\n\
                     \x20       return CreatedAtAction(nameof(Get), new { id = 1 }, body);\n    }\n\
                     \x20   [HttpDelete(\"{id}\")]\n\
                     \x20   public async Task<IActionResult> Remove(int id)\n    {\n\
                     \x20       return NoContent();\n    }\n}\n",
                ),
                (
                    "ItemDto.cs",
                    "public class ItemDto\n{\n    public string Name { get; set; }\n\
                     \x20   public int Size { get; set; }\n    public string? Note { get; set; }\n}\n",
                ),
            ],
        );
        let get = source.responses.get("Get").expect("stated");
        assert_eq!(get.statuses[&200], WireShape::Named("ItemDto".into()));
        assert_eq!(
            get.statuses[&404],
            WireShape::Unknown,
            "NotFound() states no body"
        );
        let create = source.responses.get("Create").expect("stated");
        assert_eq!(
            create.statuses[&201],
            WireShape::Named("ItemDto".into()),
            "[ProducesResponseType] types what the helper alone cannot"
        );
        let remove = source.responses.get("Remove").expect("stated");
        assert_eq!(remove.statuses[&204], WireShape::Unknown);
        // ASP.NET writes camelCase by default, so the wire names are not the
        // C# spellings.
        let dto = source.serializers.get("ItemDto").expect("collected");
        assert_eq!(
            dto["name"].shape,
            WireShape::Unknown,
            "a reference property may write null, so it claims presence, not type"
        );
        assert_eq!(dto["size"].shape, WireShape::Primitive("integer"));
        assert_eq!(
            dto["note"].shape,
            WireShape::Unknown,
            "nullable claims no type"
        );
        assert!(
            dto["note"].required,
            "null is written, so the property is present"
        );
    }

    #[test]
    fn a_plain_return_type_is_the_stated_default_and_string_abstains() {
        let source = read_source(
            "plain-returns",
            &[(
                "C.cs",
                "[Route(\"api/things\")]\npublic class ThingsController : ControllerBase\n{\n\
                 \x20   [HttpGet]\n\
                 \x20   public IEnumerable<ItemDto> List() => _items;\n\
                 \x20   [HttpGet(\"name\")]\n\
                 \x20   public string GetName() => \"x\";\n}\n",
            )],
        );
        let list = source.responses.get("List").expect("stated");
        assert_eq!(
            list.statuses[&200],
            WireShape::Array(Box::new(WireShape::Named("ItemDto".into())))
        );
        let name = source.responses.get("GetName").expect("stated");
        assert_eq!(
            name.statuses[&200],
            WireShape::Unknown,
            "a string return is text/plain, not a JSON claim"
        );
    }

    #[test]
    fn the_wire_side_reads_records_and_abstains_on_inheritance() {
        let source = read_source(
            "wire-honesty",
            &[
                (
                    "OrderDto.cs",
                    "public record OrderDto(Guid Id, List<ItemDto> Items);\n",
                ),
                (
                    "Sub.cs",
                    "public class Sub : Base\n{\n    public string Own { get; set; }\n}\n",
                ),
                (
                    "Marked.cs",
                    "public class Marked\n{\n    [JsonPropertyName(\"item_id\")]\n\
                     \x20   public string ItemId { get; set; }\n    [JsonIgnore]\n\
                     \x20   public string Secret { get; set; }\n\
                     \x20   public static string Counter { get; set; }\n}\n",
                ),
            ],
        );
        let order = source.serializers.get("OrderDto").expect("collected");
        assert_eq!(
            order["id"].shape,
            WireShape::Primitive("string"),
            "a Guid is never null"
        );
        assert_eq!(
            order["items"].shape,
            WireShape::Unknown,
            "a collection is a reference: null is possible, so no type claim"
        );
        assert!(
            !source.serializers.contains_key("Sub"),
            "a base list may inherit unreadable properties, so the class abstains"
        );
        let marked = source.serializers.get("Marked").expect("collected");
        assert!(marked.contains_key("item_id"), "the wire name wins");
        assert!(
            !marked.contains_key("secret"),
            "[JsonIgnore] never serializes"
        );
        assert!(!marked.contains_key("counter"), "static never serializes");
    }
}
