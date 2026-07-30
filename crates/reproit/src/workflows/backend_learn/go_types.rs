//! What a Go handler accepts and returns, read from the same parse the route
//! reader walks.
//!
//! Split from `go_ast` at the same boundary as `rust_router`/`rust_types`:
//! that file resolves WHERE a request lands, this one resolves WHAT the
//! handler accepts once it does and what it states it writes back. Requests
//! come from bind calls and struct tags; responses come from the write calls
//! themselves (`c.JSON(status, value)`, `WriteHeader` + `Encode`) and the
//! serializer structs they name. Everything stops at what the source states:
//! a status behind an unreadable constant and a body behind an untyped map
//! are recorded as unknown, never guessed.

use super::field_facts::{record, FieldFact};
use super::grammar::{self, MAX_FIELDS};
use super::response_facts::{literal_status, named_status, ResponseFact, WireField, WireShape};
use std::collections::{BTreeMap, BTreeSet};
use tree_sitter::Node;

/// Struct declarations, in both vocabularies at once: request-side field
/// facts (tag rules) and response-side wire fields (types).
#[derive(Default)]
pub(super) struct Structs {
    pub(super) facts: BTreeMap<String, BTreeMap<String, FieldFact>>,
    pub(super) facts_ambiguous: BTreeSet<String>,
    pub(super) wire: BTreeMap<String, BTreeMap<String, WireField>>,
    pub(super) wire_ambiguous: BTreeSet<String>,
}

/// `type BlockRequest struct { ... }`: json/binding tags for the request side,
/// field types for the response side.
pub(super) fn collect_struct(node: Node, text: &str, structs: &mut Structs) {
    let Some(name) = grammar::field(node, text, "name") else {
        return;
    };
    let Some(body) = node
        .child_by_field_name("type")
        .filter(|ty| ty.kind() == "struct_type")
        .and_then(|ty| grammar::find(ty, "field_declaration_list"))
    else {
        return;
    };
    let mut facts = BTreeMap::new();
    let mut wire = BTreeMap::new();
    // An embedded field promotes ANOTHER type's fields to this level, and they
    // cannot be enumerated from here. A partial wire shape would report every
    // promoted field as one the type does not serialize, so the whole type
    // abstains from the response side. Same rule as serde `flatten`.
    let mut embedded = false;
    let mut declarations = Vec::new();
    grammar::children(body, &mut declarations);
    for declaration in declarations.into_iter().take(MAX_FIELDS) {
        if declaration.kind() != "field_declaration" {
            continue;
        }
        let field_name = grammar::field(declaration, text, "name");
        if field_name.is_none() {
            embedded = true;
            continue;
        }
        let tag = declaration
            .child_by_field_name("tag")
            .map(|tag| grammar::text(tag, text).trim_matches('`').to_string());
        let json = tag.as_deref().and_then(|tag| tag_value(tag, "json"));
        if let (Some(tag), Some(json)) = (tag.as_deref(), json.as_deref()) {
            let json_name = json.split(',').next().unwrap_or(json).to_string();
            if !json_name.is_empty() && json_name != "-" {
                let rules = tag_value(tag, "binding")
                    .or_else(|| tag_value(tag, "validate"))
                    .unwrap_or_default();
                facts.insert(json_name, fact(&rules));
            }
        }
        collect_wire_field(declaration, text, field_name, json.as_deref(), &mut wire);
    }
    if !facts.is_empty() {
        record(
            &mut structs.facts,
            &mut structs.facts_ambiguous,
            name.clone(),
            facts,
        );
    }
    if !wire.is_empty() && !embedded {
        record(&mut structs.wire, &mut structs.wire_ambiguous, name, wire);
    }
}

/// One struct field's wire name, shape and presence, as encoding/json states
/// them: the json tag names and omits, the Go type shapes, and only exported
/// fields serialize at all.
fn collect_wire_field(
    declaration: Node,
    text: &str,
    field_name: Option<String>,
    json: Option<&str>,
    wire: &mut BTreeMap<String, WireField>,
) {
    let Some(field_name) = field_name else { return };
    if !field_name.starts_with(|c: char| c.is_ascii_uppercase()) {
        return;
    }
    let (wire_name, omitempty) = match json {
        Some(json) => {
            let name = json.split(',').next().unwrap_or(json);
            let name = if name.is_empty() { &field_name } else { name };
            if name == "-" && !json.contains(',') {
                return;
            }
            (
                name.to_string(),
                json.split(',').any(|opt| opt == "omitempty"),
            )
        }
        None => (field_name.clone(), false),
    };
    let Some(ty) = grammar::field(declaration, text, "type") else {
        return;
    };
    // A nil pointer serializes as null, and pointers are how Go states an
    // intentionally absent value, so a bare pointer field claims no type.
    // With `omitempty` the nil case is omitted instead, and whenever the
    // field IS present it carries the pointee's type exactly.
    let pointer = ty.starts_with('*');
    let shape = if pointer && !omitempty {
        WireShape::Unknown
    } else {
        shape_of(&ty)
    };
    wire.insert(
        wire_name,
        WireField {
            shape,
            required: !omitempty,
        },
    );
}

/// The wire shape a Go type states.
///
/// A slice claims `array`: that is what the type means as a contract, and a
/// nil slice serializing as `null` is exactly the divergence the contract
/// exists to catch. A map or `any` claims only what it states: an object of
/// unknown fields, or nothing at all.
pub(super) fn shape_of(ty: &str) -> WireShape {
    let ty = ty.trim().trim_start_matches('*').trim();
    if let Some(inner) = ty.strip_prefix("[]") {
        return WireShape::Array(Box::new(shape_of(inner)));
    }
    if ty.starts_with("map[") {
        return WireShape::Object;
    }
    match ty {
        "string" => WireShape::Primitive("string"),
        "bool" => WireShape::Primitive("boolean"),
        "int" | "int8" | "int16" | "int32" | "int64" | "uint" | "uint8" | "uint16" | "uint32"
        | "uint64" | "uintptr" | "byte" | "rune" => WireShape::Primitive("integer"),
        "float32" | "float64" => WireShape::Primitive("number"),
        // time.Time marshals as an RFC 3339 string; its MarshalJSON says so.
        "time.Time" => WireShape::Primitive("string"),
        // Router-provided map aliases are objects of unstated fields.
        "gin.H" | "echo.Map" | "fiber.Map" => WireShape::Object,
        "interface{}" | "any" | "json.RawMessage" => WireShape::Unknown,
        named => {
            let bare = named.rsplit('.').next().unwrap_or(named);
            let identifier =
                !bare.is_empty() && bare.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
            if identifier {
                WireShape::Named(bare.to_string())
            } else {
                WireShape::Unknown
            }
        }
    }
}

/// One key of a struct tag: `json:"blocked_id" binding:"required"`.
fn tag_value(tag: &str, key: &str) -> Option<String> {
    let at = tag.find(&format!("{key}:\""))? + key.len() + 2;
    let rest = &tag[at..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// A `binding` / `validate` rule list: `required,oneof=user sponsor,min=-1`.
fn fact(rules: &str) -> FieldFact {
    let mut allowed = None;
    let mut low = None;
    let mut high = None;
    let mut required = false;
    for rule in rules.split(',') {
        let rule = rule.trim();
        if rule == "required" {
            required = true;
        } else if let Some(values) = rule.strip_prefix("oneof=") {
            let values: Vec<String> = values.split_whitespace().map(str::to_string).collect();
            if values.len() > 1 {
                allowed = Some(values);
            }
        } else if let Some(value) = rule.strip_prefix("min=") {
            low = grammar::number(value);
        } else if let Some(value) = rule.strip_prefix("max=") {
            high = grammar::number(value);
        } else if let Some(value) = rule.strip_prefix("gte=") {
            low = grammar::number(value);
        } else if let Some(value) = rule.strip_prefix("lte=") {
            high = grammar::number(value);
        }
    }
    // `min=-1,max=1` on the SAME rule list is one range, and the grammar splits
    // the rules but not the pairing, so the two bounds compose here.
    let range = (low.is_some() || high.is_some()).then_some((low, high));
    FieldFact {
        required,
        evidence: match (&allowed, &range) {
            (Some(_), _) => Some("a struct tag `oneof` rule".to_string()),
            (_, Some(_)) => Some("a struct tag min/max rule".to_string()),
            _ => None,
        },
        allowed,
        range,
    }
}

/// Top-level `var items []Item` declarations of one file: the typed state a
/// handler routinely serves back. Only file scope is read; a local in some
/// other function is not in this handler's scope and must not leak in.
pub(super) fn package_vars(root_node: Node, text: &str) -> BTreeMap<String, String> {
    let mut vars = BTreeMap::new();
    let mut declarations = Vec::new();
    grammar::children(root_node, &mut declarations);
    for declaration in declarations {
        if declaration.kind() != "var_declaration" {
            continue;
        }
        let mut specs = Vec::new();
        grammar::children(declaration, &mut specs);
        for spec in specs {
            if spec.kind() != "var_spec" {
                continue;
            }
            if let (Some(name), Some(ty)) = (
                grammar::field(spec, text, "name"),
                grammar::field(spec, text, "type"),
            ) {
                vars.insert(name, ty);
            }
        }
    }
    vars
}

/// `func createBlock(c *gin.Context) { ... }`: the request type it binds and
/// the responses it writes. `file_vars` carries the file's top-level typed
/// declarations; the handler's own locals shadow them.
///
/// The bind call names a LOCAL, so the request type is whatever that local was
/// declared as. Reading the declaration is the whole reason to be on a parse.
pub(super) fn collect_handler(
    node: Node,
    text: &str,
    file_vars: &BTreeMap<String, String>,
    handler_body: &mut BTreeMap<String, String>,
    handler_responses: &mut BTreeMap<String, ResponseFact>,
) {
    let Some(name) = grammar::field(node, text, "name") else {
        return;
    };
    let Some(body) = node.child_by_field_name("body") else {
        return;
    };
    let mut locals: BTreeMap<String, String> = file_vars.clone();
    let mut bound: Option<String> = None;
    let mut responses = ResponseFact::default();
    // `w.WriteHeader(status)` states the status of the NEXT body write; an
    // `Encode` with none pending is net/http's implicit 200.
    let mut pending_status: Option<u16> = None;
    grammar::walk(body, &mut |inner| match inner.kind() {
        "var_spec" => {
            if let (Some(local), Some(ty)) = (
                grammar::field(inner, text, "name"),
                grammar::field(inner, text, "type"),
            ) {
                locals.insert(local, ty);
            }
        }
        // `resp := ItemList{...}` types the local through its literal.
        "short_var_declaration" => {
            if let Some((local, ty)) = typed_short_declaration(inner, text) {
                locals.insert(local, ty);
            }
        }
        "call_expression" => {
            let callee = inner
                .child_by_field_name("function")
                .and_then(|f| grammar::field(f, text, "field"))
                .unwrap_or_default();
            let mut args = Vec::new();
            if let Some(arguments) = inner.child_by_field_name("arguments") {
                grammar::children(arguments, &mut args);
            }
            if matches!(
                callee.as_str(),
                "ShouldBindJSON" | "BindJSON" | "Bind" | "ShouldBind" | "BodyParser" | "Decode"
            ) {
                if let Some(first) = args.first() {
                    let named = grammar::text(*first, text)
                        .trim_start_matches('&')
                        .to_string();
                    // A composite literal binds its own type: `&BlockRequest{}`.
                    let ty = locals
                        .get(&named)
                        .cloned()
                        .or_else(|| named.split('{').next().map(str::to_string));
                    bound = ty.filter(|ty| !ty.is_empty());
                }
            }
            collect_response_call(
                inner,
                text,
                &callee,
                &args,
                &locals,
                &mut responses,
                &mut pending_status,
            );
        }
        _ => {}
    });
    if let Some(ty) = bound {
        handler_body.insert(name.clone(), ty);
    }
    if !responses.statuses.is_empty() {
        handler_responses.insert(name, responses);
    }
}

/// The response-writing calls a handler body states.
///
/// `c.JSON(status, value)` pairs the two in one call (gin, echo, and fiber's
/// chained `c.Status(n).JSON(v)`); net/http splits them across `WriteHeader`
/// and `Encode`. A call whose status cannot be read states nothing here.
fn collect_response_call(
    call: Node,
    text: &str,
    callee: &str,
    args: &[Node],
    locals: &BTreeMap<String, String>,
    responses: &mut ResponseFact,
    pending_status: &mut Option<u16>,
) {
    match callee {
        "JSON" | "IndentedJSON" | "PureJSON" | "SecureJSON" | "AsciiJSON" => match args {
            // gin / echo: c.JSON(http.StatusOK, value)
            [status, value] => {
                if let Some(status) = status_of(*status, text) {
                    responses.state(status, value_shape(*value, text, locals));
                }
            }
            // fiber: c.JSON(value), status 200 unless chained off c.Status(n)
            [value] => {
                let status = chained_status(call, text).unwrap_or(200);
                responses.state(status, value_shape(*value, text, locals));
            }
            _ => {}
        },
        // A status written with a non-JSON or absent body still states itself.
        "String" | "HTML" | "Data" | "NoContent" => {
            if let Some(status) = args.first().and_then(|arg| status_of(*arg, text)) {
                responses.state(status, WireShape::Unknown);
            }
        }
        "Status" | "SendStatus" => {
            if let Some(status) = args.first().and_then(|arg| status_of(*arg, text)) {
                responses.state(status, WireShape::Unknown);
            }
        }
        "WriteHeader" => {
            if let Some(status) = args.first().and_then(|arg| status_of(*arg, text)) {
                responses.state(status, WireShape::Unknown);
                *pending_status = Some(status);
            }
        }
        // json.NewEncoder(w).Encode(value)
        "Encode" => {
            if grammar::text(call, text).contains("NewEncoder") {
                if let Some(value) = args.first() {
                    let status = pending_status.take().unwrap_or(200);
                    responses.state(status, value_shape(*value, text, locals));
                }
            }
        }
        // http.Error(w, message, status): a stated status, a plain-text body.
        "Error" => {
            if let Some(status) = args.get(2).and_then(|arg| status_of(*arg, text)) {
                responses.state(status, WireShape::Unknown);
            }
        }
        _ => {}
    }
}

/// `x := Type{...}` / `x := &Type{...}` / `x := []Type{...}`: the one form of
/// short declaration whose type is stated at the site.
fn typed_short_declaration(node: Node, text: &str) -> Option<(String, String)> {
    let mut left = Vec::new();
    let mut right = Vec::new();
    grammar::children(node.child_by_field_name("left")?, &mut left);
    grammar::children(node.child_by_field_name("right")?, &mut right);
    let (name, value) = match (left.as_slice(), right.as_slice()) {
        ([name], [value]) => (*name, *value),
        _ => return None,
    };
    let literal = grammar::find(value, "composite_literal")?;
    let ty = grammar::field(literal, text, "type")?;
    Some((grammar::text(name, text).to_string(), ty))
}

/// The status one argument states: an integer literal or a named constant
/// (`http.StatusOK`). Anything else states no status this can read.
fn status_of(node: Node, text: &str) -> Option<u16> {
    match node.kind() {
        "int_literal" => literal_status(grammar::text(node, text)),
        "selector_expression" => named_status(&grammar::field(node, text, "field")?),
        "identifier" => named_status(grammar::text(node, text)),
        _ => None,
    }
}

/// The fiber `c.Status(201).JSON(v)` chain: the receiver of the JSON call is
/// itself a `Status(...)` call carrying the code.
fn chained_status(call: Node, text: &str) -> Option<u16> {
    let function = call.child_by_field_name("function")?;
    let receiver = function.child_by_field_name("operand")?;
    if receiver.kind() != "call_expression" {
        return None;
    }
    let inner = receiver.child_by_field_name("function")?;
    if grammar::field(inner, text, "field").as_deref() != Some("Status") {
        return None;
    }
    let mut args = Vec::new();
    grammar::children(receiver.child_by_field_name("arguments")?, &mut args);
    args.first().and_then(|arg| status_of(*arg, text))
}

/// The wire shape of a response value expression: a composite literal states
/// its type, an identifier states its local's declared type, a bare literal
/// states its own primitive. Everything else is unknown, not guessed.
fn value_shape(node: Node, text: &str, locals: &BTreeMap<String, String>) -> WireShape {
    match node.kind() {
        "unary_expression" => match grammar::find(node, "composite_literal") {
            Some(literal) => value_shape(literal, text, locals),
            None => WireShape::Unknown,
        },
        "composite_literal" => grammar::field(node, text, "type")
            .map(|ty| shape_of(&ty))
            .unwrap_or(WireShape::Unknown),
        "identifier" => locals
            .get(grammar::text(node, text))
            .map(|ty| shape_of(ty))
            .unwrap_or(WireShape::Unknown),
        "interpreted_string_literal" | "raw_string_literal" => WireShape::Primitive("string"),
        "int_literal" => WireShape::Primitive("integer"),
        "float_literal" => WireShape::Primitive("number"),
        "true" | "false" => WireShape::Primitive("boolean"),
        _ => WireShape::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn go_types_state_their_wire_shapes() {
        assert_eq!(shape_of("string"), WireShape::Primitive("string"));
        assert_eq!(shape_of("*int64"), WireShape::Primitive("integer"));
        assert_eq!(
            shape_of("[]models.Item"),
            WireShape::Array(Box::new(WireShape::Named("Item".into())))
        );
        assert_eq!(shape_of("map[string]int"), WireShape::Object);
        assert_eq!(shape_of("time.Time"), WireShape::Primitive("string"));
        assert_eq!(shape_of("any"), WireShape::Unknown);
        assert_eq!(shape_of("chan int"), WireShape::Unknown);
    }
}
