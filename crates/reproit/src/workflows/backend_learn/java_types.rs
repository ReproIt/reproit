//! What a Spring handler states it returns, read from the same parse the
//! route reader walks.
//!
//! Split from `java_ast` at the same boundary as `go_ast`/`go_types`: that
//! file resolves WHERE a request lands, this one resolves WHAT the handler
//! states it writes back. Statuses come from `ResponseEntity` factory calls,
//! `@ResponseStatus`, and thrown `ResponseStatusException`s; bodies come from
//! the return type and the serializer classes it names. Everything stops at
//! what the source states: a status behind an unreadable expression and a
//! body behind `Object` are unknown, never guessed.

use super::grammar;
use super::response_facts::{literal_status, named_status, ResponseFact, WireField, WireShape};
use std::collections::BTreeSet;
use tree_sitter::Node;

/// How one class's fields reach the wire, computed once per class.
pub(super) struct WireContext {
    /// A class-level `@JsonInclude` makes every field's omission conditional.
    pub(super) conditional: bool,
    /// Every field serializes without its own getter: Lombok, or a record.
    pub(super) exposed: bool,
    /// Method names of the class, for the per-field getter gate.
    pub(super) getters: BTreeSet<String>,
}

impl WireContext {
    /// Jackson serializes through getters, not fields. A private field with
    /// no `getX()`/`isX()` and no Lombok never reaches the wire, and claiming
    /// it would report a field the response cannot contain.
    pub(super) fn serializes(&self, node: Node, text: &str, field: &str) -> bool {
        if self.exposed || has_modifier(node, text, &["public"]) {
            return true;
        }
        let mut capitalized = field.to_string();
        if let Some(first) = capitalized.get_mut(0..1) {
            first.make_ascii_uppercase();
        }
        self.getters.contains(&format!("get{capitalized}"))
            || self.getters.contains(&format!("is{capitalized}"))
    }
}

/// Whether a declaration's modifier list carries one of these keywords.
pub(super) fn has_modifier(node: Node, text: &str, keywords: &[&str]) -> bool {
    let Some(modifiers) = node
        .child_by_field_name("modifiers")
        .or_else(|| grammar::find(node, "modifiers"))
    else {
        return false;
    };
    grammar::text(modifiers, text)
        .split_whitespace()
        .any(|token| keywords.contains(&token))
}

/// The `ResponseEntity` factory methods that name their status in the name.
const FACTORIES: [(&str, u16); 8] = [
    ("ok", 200),
    ("created", 201),
    ("accepted", 202),
    ("noContent", 204),
    ("badRequest", 400),
    ("notFound", 404),
    ("unprocessableEntity", 422),
    ("internalServerError", 500),
];

/// What one handler method states it returns.
///
/// A plain return type is Spring's implicit 200 carrying that type, the same
/// stated default as an axum `Json<T>`; `void` states nothing unless
/// `@ResponseStatus` names a code. A `ResponseEntity<T>` types the body and
/// its factory calls name the statuses; a method that names none states
/// nothing, because a computed status is not a stated one.
pub(super) fn response_of(node: Node, text: &str, declared: Option<u16>) -> Option<ResponseFact> {
    let ty = unwrap_async(&grammar::field(node, text, "type")?);
    let mut fact = ResponseFact::default();
    if let Some(inner) = generic_inner(&ty, "ResponseEntity") {
        let stated = body_shape(&inner);
        grammar::walk(node, &mut |call| {
            entity_call(call, text, &stated, &mut fact)
        });
    } else if matches!(bare(&ty), "void" | "Void") {
        if let Some(status) = declared {
            fact.state(status, WireShape::Unknown);
        }
    } else {
        fact.state(declared.unwrap_or(200), body_shape(&ty));
    }
    grammar::walk(node, &mut |thrown| thrown_status(thrown, text, &mut fact));
    (!fact.statuses.is_empty()).then_some(fact)
}

/// One serializer field as Jackson writes it: present unless a
/// `@JsonInclude` makes omission conditional, and typed only when the type
/// can never be null. Every reference field may serialize as `null` (a fresh
/// `Item` writes `{"name":null}`, verified live), so a typed claim there is
/// a schema a healthy response violates. Same rule as a Go bare-pointer
/// field. Only an unboxed primitive is safe to type.
pub(super) fn wire_field(ty: &str, conditional: bool) -> WireField {
    let shape = match bare(ty) {
        "int" | "long" | "short" | "byte" | "char" | "boolean" | "float" | "double" => shape_of(ty),
        _ => WireShape::Unknown,
    };
    WireField {
        shape,
        required: !conditional,
    }
}

/// The wire shape a Java type states under Jackson's Spring Boot defaults
/// (java.time renders ISO-8601 strings there). `java.util.Date` is left
/// unknown: its rendering flips on a mapper flag this cannot see.
pub(super) fn shape_of(ty: &str) -> WireShape {
    let ty = ty.trim();
    if let Some(inner) = ty.strip_suffix("[]") {
        return WireShape::Array(Box::new(shape_of(inner)));
    }
    for wrapper in ["List", "Set", "Collection", "Iterable", "ArrayList"] {
        if let Some(inner) = generic_inner(ty, wrapper) {
            return WireShape::Array(Box::new(shape_of(&inner)));
        }
    }
    match bare(ty) {
        "Map" | "HashMap" | "TreeMap" | "LinkedHashMap" => WireShape::Object,
        "String" | "char" | "Character" | "UUID" | "Instant" | "LocalDate" | "LocalDateTime"
        | "LocalTime" | "OffsetDateTime" | "ZonedDateTime" | "Duration" => {
            WireShape::Primitive("string")
        }
        "int" | "Integer" | "long" | "Long" | "short" | "Short" | "byte" | "Byte"
        | "BigInteger" => WireShape::Primitive("integer"),
        "double" | "Double" | "float" | "Float" | "BigDecimal" => WireShape::Primitive("number"),
        "boolean" | "Boolean" => WireShape::Primitive("boolean"),
        named => {
            let identifier = !named.is_empty()
                && !named.contains('<')
                && named.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
            if identifier && named != "Object" && named != "Void" {
                WireShape::Named(named.to_string())
            } else {
                WireShape::Unknown
            }
        }
    }
}

/// The status an argument or annotation text names: an `HttpStatus` constant
/// or an integer literal. Anything else names no status this can read.
pub(super) fn status_of(raw: &str) -> Option<u16> {
    let raw = raw.trim();
    match raw.rsplit_once("HttpStatus.") {
        Some((_, constant)) => named_status(constant),
        None => literal_status(raw),
    }
}

/// One `ResponseEntity` expression: a factory naming its status, the
/// `.body(x)` / `.build()` chained onto it, or the two-argument constructor.
/// The preorder walk sees a chain twice, once as the pair and once as the
/// bare factory, and `ResponseFact::state` keeps the stated body.
fn entity_call(node: Node, text: &str, stated: &WireShape, fact: &mut ResponseFact) {
    if node.kind() == "object_creation_expression" {
        // new ResponseEntity<>(body, HttpStatus.CREATED)
        let ty = grammar::field(node, text, "type").unwrap_or_default();
        if bare(&ty) != "ResponseEntity" {
            return;
        }
        let args = arguments_of(node);
        let Some(status) = args
            .last()
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
    if node.kind() != "method_invocation" {
        return;
    }
    let Some(name) = grammar::field(node, text, "name") else {
        return;
    };
    match name.as_str() {
        "body" => {
            if let Some(status) = chained_factory(node, text) {
                fact.state(status, stated.clone());
            }
        }
        "build" => {
            if let Some(status) = chained_factory(node, text) {
                fact.state(status, WireShape::Unknown);
            }
        }
        _ => {
            if let Some(status) = factory_status(node, text, &name) {
                // `ok(body)` is the one factory that takes the body directly;
                // the others take a location or nothing.
                let body = if name == "ok" && !arguments_of(node).is_empty() {
                    stated.clone()
                } else {
                    WireShape::Unknown
                };
                fact.state(status, body);
            }
        }
    }
}

/// The status a direct `ResponseEntity.<factory>(...)` invocation names.
fn factory_status(node: Node, text: &str, name: &str) -> Option<u16> {
    let object = grammar::field(node, text, "object")?;
    if bare(&object) != "ResponseEntity" {
        return None;
    }
    if name == "status" {
        let args = arguments_of(node);
        return status_of(grammar::text(*args.first()?, text));
    }
    FACTORIES
        .into_iter()
        .find(|(known, _)| *known == name)
        .map(|(_, status)| status)
}

/// The factory status somewhere in the receiver of a `.body(x)` / `.build()`,
/// past whatever else is chained between them (`.headers(h)`).
fn chained_factory(node: Node, text: &str) -> Option<u16> {
    let object = node.child_by_field_name("object")?;
    let mut status = None;
    grammar::walk(object, &mut |inner| {
        if status.is_some() || inner.kind() != "method_invocation" {
            return;
        }
        if let Some(name) = grammar::field(inner, text, "name") {
            status = factory_status(inner, text, &name);
        }
    });
    status
}

/// `throw new ResponseStatusException(HttpStatus.NOT_FOUND, ...)` states its
/// status as surely as a return does; the body it writes is Spring's error
/// rendering, which this cannot claim.
fn thrown_status(node: Node, text: &str, fact: &mut ResponseFact) {
    if node.kind() != "object_creation_expression" {
        return;
    }
    let ty = grammar::field(node, text, "type").unwrap_or_default();
    if bare(&ty) != "ResponseStatusException" {
        return;
    }
    let args = arguments_of(node);
    if let Some(status) = args
        .first()
        .and_then(|arg| status_of(grammar::text(*arg, text)))
    {
        fact.state(status, WireShape::Unknown);
    }
}

/// The body shape a return or generic type states. A top-level `String` is
/// text/plain in Spring, not a JSON string, so it states its status with no
/// body claim rather than a schema the wire would falsify.
fn body_shape(ty: &str) -> WireShape {
    match bare(ty) {
        "String" | "Object" | "Void" | "?" | "" => WireShape::Unknown,
        _ => shape_of(ty),
    }
}

/// Unwrap the async and reactive wrappers a Spring signature may carry:
/// the wrapped type is what reaches the wire, and a `Flux<T>` renders as a
/// JSON array of T.
fn unwrap_async(ty: &str) -> String {
    for wrapper in ["CompletableFuture", "Mono"] {
        if let Some(inner) = generic_inner(ty, wrapper) {
            return unwrap_async(&inner);
        }
    }
    if let Some(inner) = generic_inner(ty, "Flux") {
        return format!("List<{inner}>");
    }
    ty.trim().to_string()
}

/// The generic argument of `Wrapper<T>`, or None when the type is not that
/// wrapper. The qualifier is dropped first so `http.ResponseEntity<T>` reads.
fn generic_inner(ty: &str, wrapper: &str) -> Option<String> {
    let ty = ty.trim();
    let (head, rest) = ty.split_once('<')?;
    let head = head.rsplit('.').next().unwrap_or(head).trim();
    if head != wrapper {
        return None;
    }
    Some(rest.strip_suffix('>')?.trim().to_string())
}

/// The bare head of a type: qualifier and generic arguments dropped.
fn bare(ty: &str) -> &str {
    let head = ty.trim().split('<').next().unwrap_or("").trim();
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
    fn java_types_state_their_wire_shapes() {
        assert_eq!(shape_of("String"), WireShape::Primitive("string"));
        assert_eq!(shape_of("Integer"), WireShape::Primitive("integer"));
        assert_eq!(
            shape_of("java.util.List<Item>"),
            WireShape::Array(Box::new(WireShape::Named("Item".into())))
        );
        assert_eq!(shape_of("Map<String, Object>"), WireShape::Object);
        assert_eq!(
            shape_of("Item[]"),
            WireShape::Array(Box::new(WireShape::Named("Item".into())))
        );
        assert_eq!(
            shape_of("Object"),
            WireShape::Unknown,
            "no claim behind Object"
        );
        assert_eq!(shape_of("Instant"), WireShape::Primitive("string"));
    }

    #[test]
    fn a_status_argument_reads_constants_and_literals_only() {
        assert_eq!(status_of("HttpStatus.CREATED"), Some(201));
        assert_eq!(
            status_of("org.springframework.http.HttpStatus.NOT_FOUND"),
            Some(404)
        );
        assert_eq!(status_of("(code = HttpStatus.NO_CONTENT)"), Some(204));
        assert_eq!(status_of("204"), Some(204));
        assert_eq!(
            status_of("statusVariable"),
            None,
            "a variable states nothing"
        );
    }

    use super::super::grammar::SourceRead;

    fn read_source(case: &str, files: &[(&str, &str)]) -> SourceRead {
        let root =
            std::env::temp_dir().join(format!("reproit-javatypes-{}-{case}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("root");
        for (name, body) in files {
            std::fs::write(root.join(name), body).expect("write");
        }
        let source = super::super::java_ast::read(&root);
        let _ = std::fs::remove_dir_all(&root);
        source
    }

    #[test]
    fn response_entity_factories_state_status_and_body() {
        let source = read_source(
            "responses",
            &[
                (
                    "ItemController.java",
                    "@RestController\n@RequestMapping(\"/items\")\npublic class ItemController {\n\
                     \x20   @GetMapping(\"/{id}\")\n\
                     \x20   public ResponseEntity<Item> get(@PathVariable long id) {\n\
                     \x20       if (id == 0) { return ResponseEntity.notFound().build(); }\n\
                     \x20       return ResponseEntity.ok(item);\n    }\n\
                     \x20   @PostMapping\n\
                     \x20   public ResponseEntity<Item> create(@RequestBody Item body) {\n\
                     \x20       return ResponseEntity.status(HttpStatus.CREATED).body(body);\n\
                     \x20   }\n}\n",
                ),
                (
                    "Item.java",
                    "@Data\npublic class Item {\n    private String name;\n    private int size;\n}\n",
                ),
            ],
        );
        let get = source.responses.get("get").expect("stated");
        assert_eq!(get.statuses[&200], WireShape::Named("Item".into()));
        assert_eq!(
            get.statuses[&404],
            WireShape::Unknown,
            "build() states no body"
        );
        let create = source.responses.get("create").expect("stated");
        assert_eq!(create.statuses[&201], WireShape::Named("Item".into()));
        let item = source.serializers.get("Item").expect("collected");
        assert_eq!(
            item["name"].shape,
            WireShape::Unknown,
            "a reference field may write null, so it claims presence, not type"
        );
        assert_eq!(item["size"].shape, WireShape::Primitive("integer"));
        assert!(
            item["name"].required,
            "Jackson writes the key even for null"
        );
    }

    #[test]
    fn a_plain_return_type_is_the_stated_default_and_void_states_nothing() {
        let source = read_source(
            "plain-returns",
            &[(
                "C.java",
                "@RestController\nclass C {\n\
                 \x20 @GetMapping(\"/list\") public List<Item> list() { return items; }\n\
                 \x20 @PostMapping(\"/items\") @ResponseStatus(HttpStatus.CREATED)\n\
                 \x20 public Item create(@RequestBody Item body) { return body; }\n\
                 \x20 @DeleteMapping(\"/items\") public void remove() { }\n\
                 \x20 @GetMapping(\"/name\") public String name() { return \"x\"; }\n\
                 \x20 @GetMapping(\"/missing\") public Item find() {\n\
                 \x20     throw new ResponseStatusException(HttpStatus.NOT_FOUND, \"gone\");\n\
                 \x20 }\n}\n",
            )],
        );
        let list = source.responses.get("list").expect("stated");
        assert_eq!(
            list.statuses[&200],
            WireShape::Array(Box::new(WireShape::Named("Item".into())))
        );
        let create = source.responses.get("create").expect("stated");
        assert_eq!(
            create.statuses.keys().collect::<Vec<_>>(),
            vec![&201],
            "@ResponseStatus replaces the implicit 200"
        );
        assert!(
            !source.responses.contains_key("remove"),
            "void with no @ResponseStatus states nothing"
        );
        let name = source.responses.get("name").expect("stated");
        assert_eq!(
            name.statuses[&200],
            WireShape::Unknown,
            "a String return is text/plain, not a JSON claim"
        );
        let find = source.responses.get("find").expect("stated");
        assert_eq!(
            find.statuses[&404],
            WireShape::Unknown,
            "a thrown status is stated"
        );
    }

    #[test]
    fn the_wire_side_abstains_where_it_cannot_read_the_output() {
        let source = read_source(
            "wire-honesty",
            &[
                (
                    "Sub.java",
                    "public class Sub extends Base {\n    private String own;\n\
                     \x20   public String getOwn() { return own; }\n}\n",
                ),
                (
                    "Partial.java",
                    "public class Partial {\n    private String seen;\n    private String hidden;\n\
                     \x20   public String getSeen() { return seen; }\n}\n",
                ),
                (
                    "Renamed.java",
                    "@Data\npublic class Renamed {\n    @JsonProperty(\"item_id\")\n\
                     \x20   private String itemId;\n    @JsonIgnore\n    private String secret;\n\
                     \x20   @JsonInclude(JsonInclude.Include.NON_NULL)\n    private String note;\n\
                     \x20   private static String COUNTER;\n}\n",
                ),
            ],
        );
        assert!(
            !source.serializers.contains_key("Sub"),
            "a superclass promotes unreadable fields, so the class abstains"
        );
        let partial = source.serializers.get("Partial").expect("collected");
        assert!(partial.contains_key("seen"));
        assert!(
            !partial.contains_key("hidden"),
            "no getter, no Lombok: the field never reaches the wire"
        );
        let renamed = source.serializers.get("Renamed").expect("collected");
        assert!(renamed.contains_key("item_id"), "the wire name wins");
        assert!(
            !renamed.contains_key("secret"),
            "@JsonIgnore never serializes"
        );
        assert!(!renamed.contains_key("COUNTER"), "static never serializes");
        assert!(
            !renamed["note"].required,
            "@JsonInclude makes omission conditional"
        );
    }
}
