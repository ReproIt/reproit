//! What an ASP.NET controller action states it returns, read from the same
//! parse the route reader walks.
//!
//! Split from `dotnet_ast` at the same boundary as `go_ast`/`go_types`: that
//! file resolves WHERE a request lands, this one resolves WHAT the action
//! states it writes back. Statuses come from the `ControllerBase` helper
//! calls (`Ok(...)`, `NotFound()`, `CreatedAtAction(...)`) and from
//! `[ProducesResponseType]` declarations; bodies come from the
//! `ActionResult<T>` generic and the serializer classes it names. Everything
//! stops at what the source states: a status behind an unreadable expression
//! and a body behind `object` are unknown, never guessed.

use super::grammar;
use super::response_facts::{literal_status, named_status, ResponseFact, WireField, WireShape};
use tree_sitter::Node;

/// The `ControllerBase` helpers that name their status in the method name.
const HELPERS: [(&str, u16); 14] = [
    ("Ok", 200),
    ("Created", 201),
    ("CreatedAtAction", 201),
    ("CreatedAtRoute", 201),
    ("Accepted", 202),
    ("AcceptedAtAction", 202),
    ("AcceptedAtRoute", 202),
    ("NoContent", 204),
    ("BadRequest", 400),
    ("Unauthorized", 401),
    ("Forbid", 403),
    ("NotFound", 404),
    ("Conflict", 409),
    ("UnprocessableEntity", 422),
];

/// What one controller action states it returns.
///
/// A plain return type is ASP.NET's implicit 200 carrying that type, the same
/// stated default as an axum `Json<T>`; `void` and bare `Task` state nothing.
/// An `ActionResult<T>` types the body and the helper calls name the
/// statuses; an action naming none states nothing, because a computed status
/// is not a stated one. `[ProducesResponseType]` is the developer declaring
/// the pair outright, and it is read as exactly that.
pub(super) fn response_of(
    node: Node,
    text: &str,
    attributes: &[(String, Option<String>)],
) -> Option<ResponseFact> {
    let ty = unwrap_async(&grammar::field(node, text, "returns")?);
    let mut fact = ResponseFact::default();
    for (attribute, argument) in attributes {
        produces(attribute, argument.as_deref(), &mut fact);
    }
    if let Some(inner) = generic_inner(&ty, "ActionResult") {
        let stated = body_shape(&inner);
        grammar::walk(node, &mut |call| {
            helper_call(call, text, &stated, &mut fact)
        });
    } else if matches!(
        bare(&ty),
        "IActionResult" | "ActionResult" | "IResult" | "IHttpActionResult"
    ) {
        grammar::walk(node, &mut |call| {
            helper_call(call, text, &WireShape::Unknown, &mut fact)
        });
    } else if !matches!(bare(&ty), "void" | "Task" | "ValueTask") {
        fact.state(200, body_shape(&ty));
    }
    (!fact.statuses.is_empty()).then_some(fact)
}

/// One serializer property as System.Text.Json writes it: present unless a
/// conditional `[JsonIgnore(...)]` may omit it, and typed only when the type
/// can never be null. A `T?` writes null when the value is, and a bare
/// reference type is only non-null by annotation the runtime does not
/// enforce (`new ItemDto()` writes `{"name":null}` under `#nullable
/// enable`), so only a value type is safe to type. Same rule as a Go
/// bare-pointer field.
pub(super) fn wire_field(ty: &str, conditional: bool) -> WireField {
    let trimmed = ty.trim();
    let shape = if trimmed.ends_with('?') {
        WireShape::Unknown
    } else {
        match bare(trimmed) {
            "int" | "long" | "short" | "byte" | "sbyte" | "uint" | "ulong" | "ushort" | "char"
            | "bool" | "double" | "float" | "decimal" | "Guid" | "DateTime" | "DateTimeOffset"
            | "DateOnly" | "TimeOnly" | "TimeSpan" => shape_of(trimmed),
            _ => WireShape::Unknown,
        }
    };
    WireField {
        shape,
        required: !conditional,
    }
}

/// The wire shape a C# type states under System.Text.Json's default mapping.
pub(super) fn shape_of(ty: &str) -> WireShape {
    let ty = ty.trim().trim_end_matches('?').trim();
    if let Some(inner) = ty.strip_suffix("[]") {
        return WireShape::Array(Box::new(shape_of(inner)));
    }
    for wrapper in [
        "List",
        "IList",
        "IEnumerable",
        "ICollection",
        "IReadOnlyList",
        "IReadOnlyCollection",
        "HashSet",
        "ISet",
        "Collection",
    ] {
        if let Some(inner) = generic_inner(ty, wrapper) {
            return WireShape::Array(Box::new(shape_of(&inner)));
        }
    }
    match bare(ty) {
        "Dictionary" | "IDictionary" | "IReadOnlyDictionary" => WireShape::Object,
        "string" | "String" | "char" | "Guid" | "DateTime" | "DateTimeOffset" | "DateOnly"
        | "TimeOnly" | "TimeSpan" | "Uri" => WireShape::Primitive("string"),
        "int" | "long" | "short" | "byte" | "sbyte" | "uint" | "ulong" | "ushort" | "Int32"
        | "Int64" => WireShape::Primitive("integer"),
        "double" | "float" | "decimal" => WireShape::Primitive("number"),
        "bool" | "Boolean" => WireShape::Primitive("boolean"),
        named => {
            let identifier = !named.is_empty()
                && !named.contains('<')
                && named.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
            if identifier && !matches!(named, "object" | "dynamic" | "JsonElement" | "JsonDocument")
            {
                WireShape::Named(named.to_string())
            } else {
                WireShape::Unknown
            }
        }
    }
}

/// One `ControllerBase` helper call: a bare identifier invocation inside the
/// action body. A member call (`Results.Ok(...)`, `service.NotFound()`) is
/// someone else's method and states nothing here.
fn helper_call(node: Node, text: &str, stated: &WireShape, fact: &mut ResponseFact) {
    if node.kind() != "invocation_expression" {
        return;
    }
    let Some(function) = node.child_by_field_name("function") else {
        return;
    };
    if function.kind() != "identifier" {
        return;
    }
    let name = grammar::text(function, text);
    let args = arguments_of(node);
    if name == "StatusCode" {
        // StatusCode(500) / StatusCode(StatusCodes.Status418ImATeapot, value)
        let Some(status) = args
            .first()
            .and_then(|arg| status_of(grammar::text(*arg, text)))
        else {
            return;
        };
        let body = if args.len() >= 2 {
            stated.clone()
        } else {
            WireShape::Unknown
        };
        fact.state(status, body);
        return;
    }
    let Some((_, status)) = HELPERS.into_iter().find(|(known, _)| *known == name) else {
        return;
    };
    // The value argument is first for `Ok(v)` and last for the CreatedAt
    // family (`CreatedAtAction(action, routeValues, v)`); a one-argument
    // `Created`/`Accepted` overload may carry a location instead, so only the
    // unambiguous arities claim the typed body.
    let carries_body = match name {
        "Ok" => !args.is_empty(),
        "Created" | "CreatedAtAction" | "CreatedAtRoute" | "Accepted" | "AcceptedAtAction"
        | "AcceptedAtRoute" => args.len() >= 2,
        _ => false,
    };
    let body = if !carries_body {
        WireShape::Unknown
    } else if *stated != WireShape::Unknown {
        stated.clone()
    } else {
        // No generic to type the body; an object creation in the argument
        // states its own type, anything else states nothing.
        args.last()
            .and_then(|arg| grammar::find(*arg, "object_creation_expression"))
            .and_then(|creation| grammar::field(creation, text, "type"))
            .map(|ty| shape_of(&ty))
            .unwrap_or(WireShape::Unknown)
    };
    fact.state(status, body);
}

/// One `[ProducesResponseType]` attribute: the declared status, and the body
/// type when the attribute names one (`typeof(T)`, or the generic form
/// `ProducesResponseType<T>`).
fn produces(attribute: &str, argument: Option<&str>, fact: &mut ResponseFact) {
    let Some(rest) = attribute.strip_prefix("ProducesResponseType") else {
        return;
    };
    let mut shape = rest
        .strip_prefix('<')
        .and_then(|inner| inner.strip_suffix('>'))
        .map(body_shape);
    let mut status = None;
    for part in argument.unwrap_or_default().split(',') {
        let part = part.trim();
        if let Some(inner) = part
            .strip_prefix("typeof(")
            .and_then(|p| p.strip_suffix(')'))
        {
            shape = Some(body_shape(inner));
        } else if let Some(code) = status_of(part) {
            status = Some(code);
        }
    }
    if let Some(status) = status {
        fact.state(status, shape.unwrap_or(WireShape::Unknown));
    }
}

/// System.Text.Json's CamelCase policy, which ASP.NET applies to every
/// property by default (`JsonSerializerDefaults.Web`): the leading uppercase
/// run is lowercased, stopping before an uppercase that starts a new word
/// (`Name` -> `name`, `URLValue` -> `urlValue`). Claiming the C# spelling
/// would flag every correct response for a name it never writes.
pub(super) fn camel_case(name: &str) -> String {
    let chars: Vec<char> = name.chars().collect();
    let mut out = String::with_capacity(name.len());
    let mut at = 0;
    while at < chars.len() {
        let current = chars[at];
        if !current.is_ascii_uppercase() {
            break;
        }
        if at > 0 && at + 1 < chars.len() && !chars[at + 1].is_ascii_uppercase() {
            break;
        }
        out.push(current.to_ascii_lowercase());
        at += 1;
    }
    out.extend(&chars[at..]);
    out
}

/// The status one argument or attribute part names: a `StatusCodes` constant,
/// an `HttpStatusCode` constant, or an integer literal. Anything else names
/// no status this can read.
fn status_of(raw: &str) -> Option<u16> {
    let raw = raw.trim();
    if let Some((_, constant)) = raw.rsplit_once("StatusCodes.Status") {
        let digits: String = constant.chars().take_while(char::is_ascii_digit).collect();
        return literal_status(&digits);
    }
    if let Some((_, constant)) = raw.rsplit_once("HttpStatusCode.") {
        return named_status(constant);
    }
    literal_status(raw)
}

/// The body shape a return or generic type states. A top-level `string` is
/// text/plain in ASP.NET, not a JSON string, so it states its status with no
/// body claim rather than a schema the wire would falsify.
fn body_shape(ty: &str) -> WireShape {
    match bare(ty) {
        "string" | "String" | "object" | "" => WireShape::Unknown,
        _ => shape_of(ty),
    }
}

/// Unwrap the async wrappers an action signature carries: the wrapped type is
/// what reaches the wire.
fn unwrap_async(ty: &str) -> String {
    for wrapper in ["Task", "ValueTask"] {
        if let Some(inner) = generic_inner(ty, wrapper) {
            return unwrap_async(&inner);
        }
    }
    ty.trim().to_string()
}

/// The generic argument of `Wrapper<T>`, or None when the type is not that
/// wrapper. The qualifier is dropped first so a namespaced spelling reads.
fn generic_inner(ty: &str, wrapper: &str) -> Option<String> {
    let ty = ty.trim();
    let (head, rest) = ty.split_once('<')?;
    let head = head.rsplit('.').next().unwrap_or(head).trim();
    if head != wrapper {
        return None;
    }
    Some(rest.strip_suffix('>')?.trim().to_string())
}

/// The bare head of a type: qualifier, generic arguments and `?` dropped.
fn bare(ty: &str) -> &str {
    let head = ty
        .trim()
        .trim_end_matches('?')
        .split('<')
        .next()
        .unwrap_or("")
        .trim();
    head.rsplit('.').next().unwrap_or(head)
}

fn arguments_of(node: Node) -> Vec<Node> {
    let mut args = Vec::new();
    if let Some(arguments) = node.child_by_field_name("arguments") {
        grammar::children(arguments, &mut args);
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csharp_types_state_their_wire_shapes() {
        assert_eq!(shape_of("string"), WireShape::Primitive("string"));
        assert_eq!(shape_of("int"), WireShape::Primitive("integer"));
        assert_eq!(
            shape_of("IEnumerable<ItemDto>"),
            WireShape::Array(Box::new(WireShape::Named("ItemDto".into())))
        );
        assert_eq!(shape_of("Dictionary<string, int>"), WireShape::Object);
        assert_eq!(shape_of("Guid"), WireShape::Primitive("string"));
        assert_eq!(
            shape_of("object"),
            WireShape::Unknown,
            "no claim behind object"
        );
    }

    #[test]
    fn a_status_argument_reads_every_spelling_the_framework_uses() {
        assert_eq!(status_of("StatusCodes.Status201Created"), Some(201));
        assert_eq!(status_of("(int)HttpStatusCode.NotFound"), Some(404));
        assert_eq!(status_of("500"), Some(500));
        assert_eq!(
            status_of("statusVariable"),
            None,
            "a variable states nothing"
        );
    }

    #[test]
    fn camel_case_matches_the_serializer_policy() {
        assert_eq!(camel_case("Name"), "name");
        assert_eq!(camel_case("ItemId"), "itemId");
        assert_eq!(camel_case("URLValue"), "urlValue");
        assert_eq!(camel_case("ID"), "id");
        assert_eq!(camel_case("already"), "already");
    }

    #[test]
    fn a_nullable_property_claims_no_type() {
        let field = wire_field("string?", false);
        assert_eq!(field.shape, WireShape::Unknown);
        assert!(field.required, "null is written, so the field is present");
    }
}
