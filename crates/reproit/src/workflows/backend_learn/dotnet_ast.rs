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

use super::extract::Family;
use super::field_facts::{drop_ambiguous, record, FieldFact};
use super::grammar::{self, SourceRead, MAX_FIELDS};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use tree_sitter::Node;

/// `MapGet` and `[HttpGet]` both name the verb in their suffix.
const METHODS: [&str; 7] = ["get", "post", "put", "patch", "delete", "head", "options"];

pub(super) fn read(root: &Path) -> SourceRead {
    let mut source = SourceRead::default();
    let mut shapes: BTreeMap<String, BTreeMap<String, FieldFact>> = BTreeMap::new();
    let mut ambiguous: BTreeSet<String> = BTreeSet::new();
    let mut handler_body: BTreeMap<String, String> = BTreeMap::new();
    let mut found: Vec<(String, &'static str, Option<String>)> = Vec::new();

    grammar::read_files(
        root,
        Family::DotNet,
        tree_sitter_c_sharp::LANGUAGE.into(),
        &mut source,
        |root_node, text| {
            grammar::walk(root_node, &mut |node| match node.kind() {
                "invocation_expression" => minimal_api(node, text, &mut found),
                "class_declaration" => collect_class(
                    node,
                    text,
                    &mut shapes,
                    &mut ambiguous,
                    &mut handler_body,
                    &mut found,
                ),
                _ => {}
            });
        },
    );
    drop_ambiguous(&mut shapes, &ambiguous);

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

/// `app.MapGet("/health", handler)` and `app.MapPost("/items", ...)`.
fn minimal_api(node: Node, text: &str, found: &mut Vec<(String, &'static str, Option<String>)>) {
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
    let Some(path) = args
        .first()
        .map(|arg| grammar::unquote(grammar::text(*arg, text)).to_string())
        .filter(|path| path.starts_with('/'))
    else {
        return;
    };
    found.push((path, method, None));
}

/// A controller class, or a DTO whose properties carry validation attributes.
fn collect_class(
    node: Node,
    text: &str,
    shapes: &mut BTreeMap<String, BTreeMap<String, FieldFact>>,
    ambiguous: &mut BTreeSet<String>,
    handler_body: &mut BTreeMap<String, String>,
    found: &mut Vec<(String, &'static str, Option<String>)>,
) {
    let Some(name) = grammar::field(node, text, "name") else {
        return;
    };
    // `[Route("api/users")]` prefixes this class body only. `[controller]` is
    // substituted by ASP.NET with the class name minus the Controller suffix.
    let prefix = attributes_of(node, text)
        .into_iter()
        .find(|(attribute, _)| attribute == "Route")
        .and_then(|(_, argument)| argument)
        .map(|route| substitute_tokens(&route, &name))
        .unwrap_or_default();
    let Some(body) = node.child_by_field_name("body") else {
        return;
    };
    let mut members = Vec::new();
    grammar::children(body, &mut members);
    let mut fields = BTreeMap::new();
    for member in members {
        match member.kind() {
            "method_declaration" => collect_action(member, text, &prefix, handler_body, found),
            "property_declaration" | "field_declaration" => {
                collect_property(member, text, &mut fields)
            }
            _ => {}
        }
    }
    if !fields.is_empty() {
        record(shapes, ambiguous, name, fields);
    }
}

/// `[HttpGet("{id}")] public IActionResult Get(int id)`.
fn collect_action(
    node: Node,
    text: &str,
    prefix: &str,
    handler_body: &mut BTreeMap<String, String>,
    found: &mut Vec<(String, &'static str, Option<String>)>,
) {
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
    let path = match argument {
        Some(suffix) if !suffix.is_empty() => join(prefix, &suffix),
        // A bare `[HttpGet]` serves the class route itself.
        _ => join(prefix, ""),
    };
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
                handler_body.insert(handler.clone(), bare_type(grammar::text(*ty, text)));
            }
        }
    }
    found.push((path, method, Some(handler)));
}

/// A DTO property and whatever its data-annotation attributes constrain.
fn collect_property(node: Node, text: &str, fields: &mut BTreeMap<String, FieldFact>) {
    if fields.len() >= MAX_FIELDS {
        return;
    }
    let name = grammar::field(node, text, "name").or_else(|| {
        node.child_by_field_name("declarator")
            .and_then(|declarator| grammar::field(declarator, text, "name"))
    });
    let Some(name) = name else { return };
    let mut fact = FieldFact::default();
    for (attribute, argument) in attributes_of(node, text) {
        match attribute.as_str() {
            "Required" => fact.required = true,
            "Range" => {
                let bounds: Vec<Option<f64>> = argument
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
    let wire = attributes_of(node, text)
        .into_iter()
        .find(|(attribute, _)| attribute == "JsonPropertyName")
        .and_then(|(_, argument)| argument)
        .unwrap_or(name);
    fields.insert(wire, fact);
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

fn join(prefix: &str, suffix: &str) -> String {
    let base = format!("/{}", prefix.trim_matches('/'));
    let suffix = suffix.trim_matches('/');
    if suffix.is_empty() {
        base
    } else if base == "/" {
        format!("/{suffix}")
    } else {
        format!("{base}/{suffix}")
    }
}

fn bare_type(raw: &str) -> String {
    raw.rsplit('.').next().unwrap_or(raw).trim().to_string()
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
}
