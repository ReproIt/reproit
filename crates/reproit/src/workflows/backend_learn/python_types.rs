//! What a Python handler states it returns, read from the same parse the route
//! reader walks.
//!
//! Split from `python_ast` at the same boundary as `go_ast`/`go_types`: that
//! file resolves WHERE a request lands, this one resolves WHAT the handler
//! states it writes back, plus the serializer shapes (Pydantic models and
//! dataclasses) a body may name. Statuses come from the route decorator's
//! `status_code=`, from `raise HTTPException(status_code=...)`, from
//! `JSONResponse`/`Response(status_code=...)`, and from a Flask `return body,
//! status` tuple. Bodies come from `response_model=`, a literal dict or list
//! return, and `jsonify(...)`. Everything stops at what the source states: a
//! status behind an unreadable expression and a body behind a name the reader
//! cannot type are unknown, never guessed.

use super::field_facts::{drop_ambiguous, record};
use super::grammar::{self, MAX_FIELDS};
use super::response_facts::{
    literal_status, named_status, ResponseFact, Serializers, WireField, WireShape,
};
use std::collections::{BTreeMap, BTreeSet};
use tree_sitter::Node;

/// The route decorator verbs whose decorated function is a handler. `route`
/// carries its verbs in a `methods=` keyword, but the decorated function is a
/// handler all the same.
const VERBS: [&str; 9] = [
    "get",
    "post",
    "put",
    "patch",
    "delete",
    "head",
    "options",
    "route",
    "api_route",
];

/// Response facts and serializer shapes as the file walk collects them.
///
/// A handler is keyed by its function name, which is exactly the handler the
/// route reader records, so a conflicting redefinition abstains the same way a
/// model does.
#[derive(Default)]
pub(super) struct Responses {
    facts: BTreeMap<String, ResponseFact>,
    fact_ambiguous: BTreeSet<String>,
    serializers: Serializers,
    ser_ambiguous: BTreeSet<String>,
}

impl Responses {
    /// Read one parsed file: the response facts of each routed handler and the
    /// wire shapes of each Pydantic model or dataclass.
    pub(super) fn take_file(&mut self, root: Node, text: &str) {
        grammar::walk(root, &mut |node| match node.kind() {
            "decorated_definition" => self.take_decorated(node, text),
            "class_definition" => self.take_model(node, text, false),
            _ => {}
        });
    }

    /// The per-handler response facts and the serializers they name, keeping
    /// only handlers a route actually serves and dropping every ambiguous name.
    pub(super) fn finish(
        mut self,
        routes: &[(String, &'static str, Option<String>)],
    ) -> (BTreeMap<String, ResponseFact>, Serializers) {
        drop_ambiguous(&mut self.facts, &self.fact_ambiguous);
        drop_ambiguous(&mut self.serializers, &self.ser_ambiguous);
        let handlers: BTreeSet<&String> = routes
            .iter()
            .filter_map(|(_, _, handler)| handler.as_ref())
            .collect();
        self.facts.retain(|handler, _| handlers.contains(handler));
        (self.facts, self.serializers)
    }

    /// A decorated definition: a routed function states responses, a
    /// `@dataclass` states a serializer shape.
    fn take_decorated(&mut self, node: Node, text: &str) {
        let Some(definition) = node.child_by_field_name("definition") else {
            return;
        };
        match definition.kind() {
            "function_definition" => {
                if let Some(handler) = grammar::field(definition, text, "name") {
                    if let Some(fact) = self.read_handler(node, definition, text) {
                        record(&mut self.facts, &mut self.fact_ambiguous, handler, fact);
                    }
                }
            }
            "class_definition"
                if decorator_names(node, text)
                    .iter()
                    .any(|name| name == "dataclass") =>
            {
                self.take_model(definition, text, true);
            }
            _ => {}
        }
    }

    /// The responses one routed handler states.
    ///
    /// A function with no route decorator is not a handler and states nothing
    /// here. With one, `response_model=` types the success body, the decorator's
    /// `status_code=` names its status, and the body's raises and explicit
    /// responses name the rest.
    fn read_handler(&self, decorated: Node, definition: Node, text: &str) -> Option<ResponseFact> {
        let decorators = route_decorators(decorated, text);
        if decorators.is_empty() {
            return None;
        }
        let success = decorators
            .iter()
            .find_map(|call| keyword_status(*call, text, "status_code"))
            .unwrap_or(200);
        let model = decorators
            .iter()
            .find_map(|call| keyword_value(*call, text, "response_model"))
            .map(|node| py_shape(grammar::text(node, text)));
        let mut fact = ResponseFact::default();
        if let Some(shape) = &model {
            fact.state(success, shape.clone());
        }
        let Some(body) = definition.child_by_field_name("body") else {
            return (!fact.statuses.is_empty()).then_some(fact);
        };
        grammar::walk(body, &mut |inner| match inner.kind() {
            "raise_statement" => raise_status(inner, text, &mut fact),
            "call" => explicit_response(inner, text, &mut fact),
            "return_statement" if model.is_none() => return_shape(inner, text, success, &mut fact),
            _ => {}
        });
        (!fact.statuses.is_empty()).then_some(fact)
    }

    /// A Pydantic model or dataclass: its fields as they reach the wire.
    ///
    /// `is_dataclass` is stated by the caller because a dataclass is a plain
    /// class the decorator marks; a Pydantic model is one whose bases name
    /// `BaseModel` or a marshmallow `Schema`. A class that is neither states no
    /// serializer shape.
    fn take_model(&mut self, node: Node, text: &str, is_dataclass: bool) {
        let Some(name) = grammar::field(node, text, "name") else {
            return;
        };
        let bases = grammar::field(node, text, "superclasses").unwrap_or_default();
        let pydantic = bases.contains("BaseModel") || bases.contains("Schema");
        if !pydantic && !is_dataclass {
            return;
        }
        let fields = model_wire_fields(node, text);
        if !fields.is_empty() {
            record(&mut self.serializers, &mut self.ser_ambiguous, name, fields);
        }
    }
}

/// `Field(ge=-1, le=1)`, with exclusive bounds converted to what is accepted.
///
/// Read for the request side by `python_ast`; it lives here with the other
/// type readers rather than in the route file.
pub(super) fn field_bounds(default: &str) -> Option<(Option<f64>, Option<f64>)> {
    let compact: String = default.chars().filter(|c| !c.is_whitespace()).collect();
    let mut low = None;
    let mut high = None;
    for (key, slot) in [("ge=", 0), ("gt=", 1), ("le=", 2), ("lt=", 3)] {
        let Some(value) = compact.split(key).nth(1) else {
            continue;
        };
        let literal: String = value
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '-' || *c == '.')
            .collect();
        let Ok(number) = literal.parse::<f64>() else {
            continue;
        };
        match slot {
            0 => low = Some(number),
            1 => low = Some(number + 1.0),
            2 => high = Some(number),
            _ => high = Some(number - 1.0),
        }
    }
    (low.is_some() || high.is_some()).then_some((low, high))
}

/// The wire fields a model class states.
///
/// Every field a model declares serializes, so each is present. Its type is
/// claimed only when the annotation can never be null: an `Optional[...]` field
/// may write `null`, so it claims presence, not type, the same rule as a Go
/// bare pointer and a Java reference field. A class configured `exclude_none`
/// omits its null fields, so there every field's presence is conditional.
fn model_wire_fields(node: Node, text: &str) -> BTreeMap<String, WireField> {
    let mut fields = BTreeMap::new();
    let Some(body) = node.child_by_field_name("body") else {
        return fields;
    };
    let exclude_none = grammar::text(body, text).contains("exclude_none");
    let mut cursor = body.walk();
    for statement in body.children(&mut cursor).take(MAX_FIELDS) {
        if statement.kind() != "expression_statement" {
            continue;
        }
        let Some(expression) = statement.child(0) else {
            continue;
        };
        let (name, annotation, default) = match expression.kind() {
            // `name: annotation`
            "type" | "typed_parameter" => (
                expression
                    .child_by_field_name("left")
                    .or_else(|| expression.child(0)),
                Some(expression),
                None,
            ),
            // `name: annotation = default`
            "assignment" => (
                expression.child_by_field_name("left"),
                expression.child_by_field_name("type"),
                expression.child_by_field_name("right"),
            ),
            _ => continue,
        };
        let (Some(name), Some(annotation)) = (name, annotation) else {
            continue;
        };
        let name = grammar::text(name, text);
        if !name.chars().next().is_some_and(char::is_alphabetic) {
            continue;
        }
        // A `Field(exclude=True)` field is never written, so claiming it would
        // report a key the response cannot contain.
        let default = default
            .map(|node| grammar::text(node, text))
            .unwrap_or_default();
        if default.contains("exclude=True") || default.contains("exclude =True") {
            continue;
        }
        let annotation = annotation
            .child_by_field_name("type")
            .map(|ty| grammar::text(ty, text).to_string())
            .unwrap_or_else(|| strip_field_name(grammar::text(annotation, text), name));
        let optional = is_optional(&annotation);
        let shape = if optional {
            WireShape::Unknown
        } else {
            py_shape(&annotation)
        };
        fields.insert(
            name.to_string(),
            WireField {
                shape,
                required: !exclude_none,
            },
        );
    }
    fields
}

/// The annotation text of a `name: annotation` node, with the field name
/// removed. A `type` node is `name: annotation`; the annotation is what is
/// left once the name and colon are stripped.
fn strip_field_name(node_text: &str, name: &str) -> String {
    node_text
        .trim()
        .strip_prefix(name)
        .and_then(|rest| rest.trim_start().strip_prefix(':'))
        .unwrap_or(node_text)
        .trim()
        .to_string()
}

/// Whether an annotation may be null: `Optional[...]`, `... | None`, or a bare
/// `None`.
fn is_optional(annotation: &str) -> bool {
    let annotation = annotation.trim();
    annotation.contains("Optional[")
        || annotation.contains("| None")
        || annotation.contains("None |")
        || annotation == "None"
}

/// The wire shape a Python type annotation states under a JSON serializer.
///
/// A collection claims `array`, a mapping claims `object`, the scalar types map
/// to their JSON primitive, and `datetime`/`UUID` render as strings. A
/// capitalized name that is none of these is a serializer type, resolved
/// against the collected models at emission time; an unresolved one claims
/// nothing. `Any`, a bare `dict`/`list` with no argument, and a lowercase
/// unknown claim nothing.
fn py_shape(annotation: &str) -> WireShape {
    let ty = annotation.trim();
    let ty = ty
        .strip_prefix("Optional[")
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(ty)
        .trim();
    for wrapper in [
        "List",
        "list",
        "Sequence",
        "Set",
        "set",
        "FrozenSet",
        "Iterable",
        "Tuple",
    ] {
        if let Some(inner) = generic_inner(ty, wrapper) {
            let first = inner.split(',').next().unwrap_or(&inner);
            return WireShape::Array(Box::new(py_shape(first)));
        }
    }
    for wrapper in ["Dict", "dict", "Mapping"] {
        if generic_inner(ty, wrapper).is_some() {
            return WireShape::Object;
        }
    }
    match ty {
        "str" | "EmailStr" | "AnyUrl" | "HttpUrl" | "datetime" | "date" | "time" | "UUID" => {
            WireShape::Primitive("string")
        }
        "int" => WireShape::Primitive("integer"),
        "float" => WireShape::Primitive("number"),
        "bool" => WireShape::Primitive("boolean"),
        "dict" | "Dict" | "Mapping" => WireShape::Object,
        named => {
            let bare = named.rsplit('.').next().unwrap_or(named);
            let capitalized = bare.starts_with(|c: char| c.is_ascii_uppercase())
                && bare.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
            if capitalized && bare != "Any" && bare != "Optional" {
                WireShape::Named(bare.to_string())
            } else {
                WireShape::Unknown
            }
        }
    }
}

/// The generic argument of `Wrapper[T]`, or None when the type is not that
/// wrapper.
fn generic_inner(ty: &str, wrapper: &str) -> Option<String> {
    let (head, rest) = ty.split_once('[')?;
    if head.trim() != wrapper {
        return None;
    }
    Some(rest.strip_suffix(']')?.trim().to_string())
}

/// The route decorators on a decorated definition, as their `call` nodes:
/// `@app.get("/x")`, `@router.post(...)`, `@app.route(...)`. A non-route
/// decorator (`@wraps`, `@lru_cache`) is not one of these.
fn route_decorators<'a>(node: Node<'a>, text: &str) -> Vec<Node<'a>> {
    let mut found = Vec::new();
    let mut cursor = node.walk();
    for decorator in node.children(&mut cursor) {
        if decorator.kind() != "decorator" {
            continue;
        }
        let mut inner = decorator.walk();
        let Some(call) = decorator
            .children(&mut inner)
            .find(|child| child.kind() == "call")
        else {
            continue;
        };
        let verb = call
            .child_by_field_name("function")
            .and_then(|function| grammar::field(function, text, "attribute"))
            .unwrap_or_default();
        if VERBS.contains(&verb.as_str()) {
            found.push(call);
        }
    }
    found
}

/// Every decorator's name, route or not, for the dataclass check.
fn decorator_names(node: Node, text: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut cursor = node.walk();
    for decorator in node.children(&mut cursor) {
        if decorator.kind() != "decorator" {
            continue;
        }
        // `@dataclass` and `@dataclass(frozen=True)`: the name is the identifier
        // or the callee of the call.
        let raw = grammar::text(decorator, text);
        let name: String = raw
            .trim_start_matches('@')
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '.')
            .collect();
        if let Some(last) = name.rsplit('.').next() {
            names.push(last.to_string());
        }
    }
    names
}

/// The status a `raise HTTPException(status_code=...)` states. The body is
/// Starlette's error rendering, which this cannot claim.
fn raise_status(node: Node, text: &str, fact: &mut ResponseFact) {
    let Some(call) = grammar::find(node, "call") else {
        return;
    };
    if callee_name(call, text) != "HTTPException" {
        return;
    }
    if let Some(status) =
        keyword_status(call, text, "status_code").or_else(|| first_status(call, text))
    {
        fact.state(status, WireShape::Unknown);
    }
}

/// An explicit `JSONResponse(...)` / `Response(...)` call and the status it
/// names. `JSONResponse` may carry a literal body; the plain responses do not.
fn explicit_response(node: Node, text: &str, fact: &mut ResponseFact) {
    let callee = callee_name(node, text);
    let carries_body = matches!(callee, "JSONResponse" | "ORJSONResponse" | "UJSONResponse");
    if !carries_body && !matches!(callee, "Response" | "PlainTextResponse" | "HTMLResponse") {
        return;
    }
    let Some(status) = keyword_status(node, text, "status_code") else {
        return;
    };
    let body = if carries_body {
        keyword_value(node, text, "content")
            .or_else(|| first_positional(node, text))
            .map(|value| value_shape(value, text))
            .unwrap_or(WireShape::Unknown)
    } else {
        WireShape::Unknown
    };
    fact.state(status, body);
}

/// The status and shape a `return` states.
///
/// A Flask `return body, status` tuple names its status; a single value takes
/// the handler's success status. The body is stated only for a literal the
/// reader can shape, else the status stands alone.
fn return_shape(node: Node, text: &str, success: u16, fact: &mut ResponseFact) {
    let Some(value) = node.named_child(0) else {
        return;
    };
    if matches!(value.kind(), "expression_list" | "tuple") {
        let mut cursor = value.walk();
        let parts: Vec<Node> = value.children(&mut cursor).filter(Node::is_named).collect();
        if let [body, status] = parts.as_slice() {
            if let Some(code) = expr_status(*status, text) {
                fact.state(code, value_shape(*body, text));
                return;
            }
        }
        return;
    }
    let shape = value_shape(value, text);
    // A single return of a value the reader cannot shape states no body, and a
    // handler that only ever returns such a value states no success status
    // either: claiming a bare 200 there would be a fact nobody wrote.
    if shape != WireShape::Unknown {
        fact.state(success, shape);
    }
}

/// The wire shape a returned expression states: a literal dict or list, or a
/// `jsonify(...)` call. Everything else is unknown, not guessed.
fn value_shape(node: Node, text: &str) -> WireShape {
    match node.kind() {
        "dictionary" => WireShape::Object,
        "list" | "list_comprehension" => WireShape::Array(Box::new(WireShape::Unknown)),
        "call" if callee_name(node, text) == "jsonify" => first_positional(node, text)
            .map(|value| match value.kind() {
                "list" | "list_comprehension" => WireShape::Array(Box::new(WireShape::Unknown)),
                _ => WireShape::Object,
            })
            .unwrap_or(WireShape::Object),
        _ => WireShape::Unknown,
    }
}

/// The last identifier of a call's function: `x.JSONResponse(...)` -> the tail.
fn callee_name<'a>(call: Node, text: &'a str) -> &'a str {
    let Some(function) = call.child_by_field_name("function") else {
        return "";
    };
    let raw = grammar::text(function, text);
    raw.rsplit('.').next().unwrap_or(raw)
}

/// The value node of a keyword argument, if the call states it.
fn keyword_value<'a>(call: Node<'a>, text: &str, key: &str) -> Option<Node<'a>> {
    let arguments = call.child_by_field_name("arguments")?;
    let mut children = Vec::new();
    grammar::children(arguments, &mut children);
    children
        .into_iter()
        .filter(|child| child.kind() == "keyword_argument")
        .find(|child| grammar::field(*child, text, "name").as_deref() == Some(key))
        .and_then(|child| child.child_by_field_name("value"))
}

/// The first positional argument of a call, if any.
fn first_positional<'a>(call: Node<'a>, _text: &str) -> Option<Node<'a>> {
    let arguments = call.child_by_field_name("arguments")?;
    let mut children = Vec::new();
    grammar::children(arguments, &mut children);
    children
        .into_iter()
        .find(|child| child.kind() != "keyword_argument")
}

/// The status a keyword argument names, an integer or an `HTTP_xxx` constant.
fn keyword_status(call: Node, text: &str, key: &str) -> Option<u16> {
    keyword_value(call, text, key).and_then(|value| expr_status(value, text))
}

/// The status the first positional argument names.
fn first_status(call: Node, text: &str) -> Option<u16> {
    first_positional(call, text).and_then(|value| expr_status(value, text))
}

/// The status an expression states: a bare integer, or a constant whose name
/// carries the code as digits (`status.HTTP_201_CREATED`) or as a word
/// (`HTTP_NOT_FOUND`). Anything else states no status this can read.
fn expr_status(node: Node, text: &str) -> Option<u16> {
    let raw = grammar::text(node, text).trim();
    if let Some(code) = literal_status(raw) {
        return Some(code);
    }
    let tail = raw.rsplit('.').next().unwrap_or(raw);
    // `HTTP_201_CREATED` spells the code out in digits; a three-digit run in the
    // HTTP range is the code stated explicitly.
    let digits: String = tail
        .split(|c: char| !c.is_ascii_digit())
        .find(|part| part.len() == 3)
        .unwrap_or_default()
        .to_string();
    if let Some(code) = literal_status(&digits) {
        return Some(code);
    }
    // Otherwise the word after `HTTP_` names a status the shared table knows.
    named_status(tail.trim_start_matches("HTTP_"))
}

#[cfg(test)]
mod tests {
    use super::super::grammar::SourceRead;
    use super::super::python_ast;
    use super::*;

    fn read_source(case: &str, files: &[(&str, &str)]) -> SourceRead {
        let root =
            std::env::temp_dir().join(format!("reproit-pytypes-{}-{case}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("root");
        for (name, body) in files {
            std::fs::write(root.join(name), body).expect("write");
        }
        let source = python_ast::read(&root);
        let _ = std::fs::remove_dir_all(&root);
        source
    }

    #[test]
    fn python_shapes_map_annotations_to_the_wire() {
        assert_eq!(py_shape("str"), WireShape::Primitive("string"));
        assert_eq!(py_shape("int"), WireShape::Primitive("integer"));
        assert_eq!(py_shape("datetime"), WireShape::Primitive("string"));
        assert_eq!(
            py_shape("List[Item]"),
            WireShape::Array(Box::new(WireShape::Named("Item".into())))
        );
        assert_eq!(py_shape("Dict[str, int]"), WireShape::Object);
        assert_eq!(py_shape("Any"), WireShape::Unknown);
        assert_eq!(py_shape("Item"), WireShape::Named("Item".into()));
    }

    #[test]
    fn a_response_model_states_the_success_status_and_body() {
        let source = read_source(
            "response-model",
            &[(
                "main.py",
                "from pydantic import BaseModel\nfrom typing import Optional\n\
                 class Item(BaseModel):\n    id: str\n    size: int\n    note: Optional[str] = None\n\n\
                 @app.get(\"/items/{item_id}\", response_model=Item)\n\
                 async def read_item(item_id: str):\n    return store[item_id]\n",
            )],
        );
        let fact = source.responses.get("read_item").expect("stated");
        assert_eq!(fact.statuses[&200], WireShape::Named("Item".into()));
        let item = source.serializers.get("Item").expect("collected");
        assert_eq!(item["id"].shape, WireShape::Primitive("string"));
        assert_eq!(item["size"].shape, WireShape::Primitive("integer"));
        assert_eq!(
            item["note"].shape,
            WireShape::Unknown,
            "an Optional field may write null, so it claims presence, not type"
        );
        assert!(item["note"].required, "the key is still written");
    }

    #[test]
    fn a_status_code_on_the_decorator_replaces_the_default_200() {
        let source = read_source(
            "status-code",
            &[(
                "main.py",
                "from pydantic import BaseModel\nclass Item(BaseModel):\n    id: str\n\n\
                 @app.post(\"/items\", status_code=201, response_model=Item)\n\
                 async def create(body: Item):\n    return body\n",
            )],
        );
        let fact = source.responses.get("create").expect("stated");
        assert_eq!(
            fact.statuses.keys().copied().collect::<Vec<_>>(),
            vec![201],
            "status_code replaces the implicit 200"
        );
        assert_eq!(fact.statuses[&201], WireShape::Named("Item".into()));
    }

    #[test]
    fn a_raised_http_exception_states_its_status_without_a_body() {
        let source = read_source(
            "raise",
            &[(
                "main.py",
                "from fastapi import HTTPException\n\
                 @app.get(\"/items/{item_id}\")\n\
                 async def read_item(item_id: str):\n\
                 \x20   if item_id not in store:\n\
                 \x20       raise HTTPException(status_code=404, detail=\"gone\")\n\
                 \x20   return {\"id\": item_id}\n",
            )],
        );
        let fact = source.responses.get("read_item").expect("stated");
        assert_eq!(
            fact.statuses[&404],
            WireShape::Unknown,
            "a thrown status is stated"
        );
        assert_eq!(
            fact.statuses[&200],
            WireShape::Object,
            "a literal dict is an object"
        );
    }

    #[test]
    fn a_json_response_states_its_own_status_and_literal_shape() {
        let source = read_source(
            "jsonresponse",
            &[(
                "main.py",
                "from fastapi.responses import JSONResponse\nfrom fastapi import Response\n\
                 @app.post(\"/items\")\n\
                 async def create(body: dict):\n\
                 \x20   if not body:\n        return Response(status_code=204)\n\
                 \x20   return JSONResponse({\"ok\": True}, status_code=201)\n",
            )],
        );
        let fact = source.responses.get("create").expect("stated");
        assert_eq!(
            fact.statuses[&201],
            WireShape::Object,
            "the literal content is an object"
        );
        assert_eq!(
            fact.statuses[&204],
            WireShape::Unknown,
            "a bare Response states no body"
        );
    }

    #[test]
    fn a_flask_tuple_return_states_its_status_and_jsonify_is_an_object() {
        let source = read_source(
            "flask",
            &[(
                "app.py",
                "from flask import jsonify\n\
                 @app.route(\"/things\", methods=[\"POST\"])\n\
                 def things():\n    return jsonify({\"created\": True}), 201\n",
            )],
        );
        let fact = source.responses.get("things").expect("stated");
        assert_eq!(
            fact.statuses[&201],
            WireShape::Object,
            "jsonify is a JSON object"
        );
        assert!(
            !fact.statuses.contains_key(&200),
            "the tuple states 201, not 200"
        );
    }

    #[test]
    fn an_unprovable_handler_states_no_false_status() {
        // A handler that returns a value the reader cannot shape, with no
        // response_model, status_code, raise, or explicit response, states
        // nothing rather than a fabricated 200.
        let source = read_source(
            "abstain",
            &[(
                "main.py",
                "@app.get(\"/items\")\nasync def read_items():\n    return serialize(store)\n",
            )],
        );
        assert!(
            !source.responses.contains_key("read_items"),
            "an unprovable handler must abstain: {:?}",
            source.responses
        );
    }

    #[test]
    fn a_dataclass_is_a_serializer_shape() {
        let source = read_source(
            "dataclass",
            &[(
                "main.py",
                "from dataclasses import dataclass\nfrom typing import Optional\n\
                 @dataclass\nclass Point:\n    x: int\n    y: int\n    label: Optional[str] = None\n\n\
                 @app.get(\"/point\", response_model=Point)\ndef point():\n    return p\n",
            )],
        );
        let point = source.serializers.get("Point").expect("collected");
        assert_eq!(point["x"].shape, WireShape::Primitive("integer"));
        assert_eq!(
            point["label"].shape,
            WireShape::Unknown,
            "Optional claims presence, not type"
        );
        let fact = source.responses.get("point").expect("stated");
        assert_eq!(fact.statuses[&200], WireShape::Named("Point".into()));
    }

    #[test]
    fn a_model_declared_differently_twice_abstains() {
        let source = read_source(
            "ambiguous",
            &[
                (
                    "main.py",
                    "from pydantic import BaseModel\nclass Item(BaseModel):\n    a: str\n    b: str\n\n\
                     @app.get(\"/x\", response_model=Item)\ndef h():\n    return i\n",
                ),
                (
                    "legacy.py",
                    "from pydantic import BaseModel\nclass Item(BaseModel):\n    a: str\n",
                ),
            ],
        );
        assert!(
            !source.serializers.contains_key("Item"),
            "two different models with one name is not a verdict: {:?}",
            source.serializers.keys().collect::<Vec<_>>()
        );
    }
}
