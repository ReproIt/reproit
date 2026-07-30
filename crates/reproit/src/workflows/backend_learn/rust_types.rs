//! What a Rust handler's request body actually accepts.
//!
//! Split from the route reader because the two answer different questions:
//! the router walk resolves WHERE a request lands, this resolves WHAT that
//! handler will accept once it does. Both read the same `syn` parse.
//!
//! Serde attributes and validation guards are read exactly, and anything not
//! recognised is left unstated rather than guessed: a field whose bound we
//! could not read must not be reported as unbounded.

use super::field_facts::FieldFact;
use super::response_facts::{named_status, ResponseFact, WireField, WireShape};
use std::collections::BTreeMap;
use syn::{Expr, Lit};

/// The `Json<T>` REQUEST body of a handler. A `Json<T>` RETURN type is not one.
pub(super) fn json_body_type(function: &syn::ItemFn) -> Option<String> {
    function.sig.inputs.iter().find_map(|input| {
        let syn::FnArg::Typed(typed) = input else {
            return None;
        };
        inner_of(&typed.ty, "Json")
    })
}

/// The single generic argument of `Wrapper<T>`, by bare name.
pub(super) fn inner_of(ty: &syn::Type, wrapper: &str) -> Option<String> {
    let syn::Type::Path(path) = ty else {
        return None;
    };
    let segment = path.path.segments.last()?;
    if segment.ident != wrapper {
        return None;
    }
    let syn::PathArguments::AngleBracketed(args) = &segment.arguments else {
        return None;
    };
    args.args.iter().find_map(|arg| match arg {
        syn::GenericArgument::Type(syn::Type::Path(inner)) => {
            Some(inner.path.segments.last()?.ident.to_string())
        }
        _ => None,
    })
}

pub(super) fn struct_fields(item: &syn::ItemStruct) -> BTreeMap<String, FieldFact> {
    let syn::Fields::Named(named) = &item.fields else {
        return BTreeMap::new();
    };
    // A `#[serde(flatten)]` field puts ANOTHER type's fields on the wire at
    // this level, and they cannot be enumerated from here. Returning a partial
    // set would report every flattened field as one the handler does not have,
    // so the whole type abstains: an unreadable shape is not an empty one.
    if named
        .named
        .iter()
        .any(|field| serde_flag(&field.attrs, "flatten"))
    {
        return BTreeMap::new();
    }
    // `rename_all` on the container renames every field, and comparing the
    // Rust name against a renamed wire name reports a present field as absent.
    let rename_all = rename_all(&item.attrs);
    // `#[serde(default)]` on the CONTAINER defaults every field in it.
    let all_default = serde_flag(&item.attrs, "default");
    let mut fields = BTreeMap::new();
    for field in &named.named {
        let Some(ident) = &field.ident else { continue };
        let name = serde_rename(&field.attrs).unwrap_or_else(|| {
            super::field_facts::apply_rename_all(&ident.to_string(), rename_all.as_deref())
        });
        // Required means "omitting it is rejected", which `Option` is only one
        // way to opt out of. `#[serde(default)]` on a non-Option field makes it
        // optional just as surely: omitting it yields the default rather than
        // an error. Reading only the type reported a correct schema as missing
        // a `required` entry, and stated a rejection that does not happen.
        let optional = inner_of(&field.ty, "Option").is_some()
            || all_default
            || serde_flag(&field.attrs, "default")
            // Never populated from input at all, so it cannot be required. It
            // stays in the set: the schema may still declare it, and sending
            // it is ignored rather than wrong.
            || serde_flag(&field.attrs, "skip_deserializing")
            || serde_flag(&field.attrs, "skip");
        let declared = inner_of(&field.ty, "Option").or_else(|| bare_type(&field.ty));
        let range = super::field_facts::attribute_range(
            &field.attrs.iter().map(quote_attr).collect::<Vec<_>>(),
        );
        fields.insert(
            name,
            FieldFact {
                required: !optional,
                // Remembered by name; resolved once every module is read.
                evidence: match &range {
                    Some(_) => Some("a validation attribute on the field".to_string()),
                    None => declared.map(|name| format!("@{name}")),
                },
                allowed: None,
                range,
            },
        );
    }
    fields
}

pub(super) fn bare_type(ty: &syn::Type) -> Option<String> {
    let syn::Type::Path(path) = ty else {
        return None;
    };
    Some(path.path.segments.last()?.ident.to_string())
}

/// The single generic type argument of `Wrapper<T>`, as a type.
fn generic_inner<'a>(ty: &'a syn::Type, wrapper: &str) -> Option<&'a syn::Type> {
    let syn::Type::Path(path) = ty else {
        return None;
    };
    let segment = path.path.segments.last()?;
    if segment.ident != wrapper {
        return None;
    }
    let syn::PathArguments::AngleBracketed(args) = &segment.arguments else {
        return None;
    };
    args.args.iter().find_map(|arg| match arg {
        syn::GenericArgument::Type(inner) => Some(inner),
        _ => None,
    })
}

/// The wire shape a Rust type states under serde's default mapping.
///
/// `Option<T>` claims nothing: `None` serializes as `null`, so the field's
/// presence is certain but its type is not. Anything unrecognised is Unknown,
/// never a guess.
pub(super) fn wire_shape_of(ty: &syn::Type) -> WireShape {
    match ty {
        syn::Type::Reference(inner) => wire_shape_of(&inner.elem),
        syn::Type::Paren(inner) => wire_shape_of(&inner.elem),
        syn::Type::Array(array) => WireShape::Array(Box::new(wire_shape_of(&array.elem))),
        syn::Type::Path(path) => {
            let Some(segment) = path.path.segments.last() else {
                return WireShape::Unknown;
            };
            let name = segment.ident.to_string();
            match name.as_str() {
                "String" | "str" | "Uuid" => WireShape::Primitive("string"),
                "bool" => WireShape::Primitive("boolean"),
                "i8" | "i16" | "i32" | "i64" | "i128" | "isize" | "u8" | "u16" | "u32" | "u64"
                | "u128" | "usize" => WireShape::Primitive("integer"),
                "f32" | "f64" => WireShape::Primitive("number"),
                "Vec" | "VecDeque" | "BTreeSet" | "HashSet" => generic_inner(ty, &name)
                    .map(|inner| WireShape::Array(Box::new(wire_shape_of(inner))))
                    .unwrap_or(WireShape::Unknown),
                "Box" | "Arc" | "Rc" | "Cow" => generic_inner(ty, &name)
                    .map(wire_shape_of)
                    .unwrap_or(WireShape::Unknown),
                "HashMap" | "BTreeMap" => WireShape::Object,
                "Option" | "Value" => WireShape::Unknown,
                other => {
                    let identifier = other
                        .chars()
                        .all(|character| character.is_ascii_alphanumeric() || character == '_');
                    if identifier {
                        WireShape::Named(other.to_string())
                    } else {
                        WireShape::Unknown
                    }
                }
            }
        }
        _ => WireShape::Unknown,
    }
}

/// The serialized fields of a struct, by wire name, or None where they cannot
/// be enumerated: a `#[serde(flatten)]` field puts another type's fields at
/// this level, so a partial set would misstate the shape.
pub(super) fn wire_fields(item: &syn::ItemStruct) -> Option<BTreeMap<String, WireField>> {
    let syn::Fields::Named(named) = &item.fields else {
        return None;
    };
    if named
        .named
        .iter()
        .any(|field| serde_flag(&field.attrs, "flatten"))
    {
        return None;
    }
    let rename_rule = rename_all(&item.attrs);
    let mut fields = BTreeMap::new();
    for field in &named.named {
        let Some(ident) = &field.ident else { continue };
        // Never on the wire at all.
        if serde_flag(&field.attrs, "skip") || serde_flag(&field.attrs, "skip_serializing") {
            continue;
        }
        let name = serde_rename(&field.attrs).unwrap_or_else(|| {
            super::field_facts::apply_rename_all(&ident.to_string(), rename_rule.as_deref())
        });
        // A custom serializer rewrites the value arbitrarily: the declared
        // type no longer states the wire shape.
        let custom = serde_flag(&field.attrs, "serialize_with")
            || serde_flag(&field.attrs, "with")
            || field
                .attrs
                .iter()
                .any(|attr| quote_attr(attr).starts_with("serde_as"));
        // `skip_serializing_if` is serde's omitempty: the field may be absent,
        // and whenever it IS present on an Option it carries the inner type.
        let optional = serde_flag(&field.attrs, "skip_serializing_if");
        let shape = if custom {
            WireShape::Unknown
        } else if optional {
            wire_shape_of(generic_inner(&field.ty, "Option").unwrap_or(&field.ty))
        } else {
            wire_shape_of(&field.ty)
        };
        fields.insert(
            name,
            WireField {
                shape,
                required: !optional,
            },
        );
    }
    (!fields.is_empty()).then_some(fields)
}

/// What a handler states it returns, from its signature and body.
///
/// A `Json<T>` return (axum, rocket) is a 200 carrying T; a `Result`'s error
/// arm states no status this can read, so only the Ok arm speaks. A
/// `(StatusCode, Json<T>)` return carries T at each `StatusCode::X` literal
/// the body names, and claims nothing when it names none. actix states both
/// in one expression: `HttpResponse::Created().json(value)`.
pub(super) fn response_of(function: &syn::ItemFn) -> Option<ResponseFact> {
    let mut visitor = ResponseCalls::default();
    syn::visit::Visit::visit_block(&mut visitor, &function.block);
    let mut fact = visitor.fact;
    if let syn::ReturnType::Type(_, ty) = &function.sig.output {
        let ty = generic_inner(ty, "Result").unwrap_or(ty);
        if let Some(body) = generic_inner(ty, "Json").map(wire_shape_of) {
            fact.state(200, body);
        } else if let syn::Type::Tuple(tuple) = ty {
            let json = tuple
                .elems
                .iter()
                .find_map(|element| generic_inner(element, "Json"))
                .map(wire_shape_of);
            if let Some(body) = json {
                for status in &visitor.status_literals {
                    fact.state(*status, body.clone());
                }
            }
        }
    }
    (!fact.statuses.is_empty()).then_some(fact)
}

/// Response-stating expressions in a handler body.
#[derive(Default)]
struct ResponseCalls {
    fact: ResponseFact,
    /// `let x: T` and `let x = T { .. }` bindings, for resolving the value an
    /// actix `.json(x)` call serializes.
    locals: BTreeMap<String, WireShape>,
    /// Every `StatusCode::X` literal the body names, for tuple returns.
    status_literals: Vec<u16>,
}

impl<'ast> syn::visit::Visit<'ast> for ResponseCalls {
    fn visit_local(&mut self, local: &'ast syn::Local) {
        if let syn::Pat::Ident(pat) = &local.pat {
            let shape = local
                .init
                .as_ref()
                .map(|init| value_shape(&init.expr, &self.locals))
                .filter(|shape| *shape != WireShape::Unknown);
            if let Some(shape) = shape {
                self.locals.insert(pat.ident.to_string(), shape);
            }
        } else if let syn::Pat::Type(typed) = &local.pat {
            if let syn::Pat::Ident(pat) = &*typed.pat {
                self.locals
                    .insert(pat.ident.to_string(), wire_shape_of(&typed.ty));
            }
        }
        syn::visit::visit_local(self, local);
    }

    fn visit_expr(&mut self, expr: &'ast Expr) {
        match expr {
            // actix: HttpResponse::Created().json(value)
            Expr::MethodCall(call) if call.method == "json" => {
                if let Some(status) = http_response_status(&call.receiver) {
                    let body = call
                        .args
                        .first()
                        .map(|arg| value_shape(arg, &self.locals))
                        .unwrap_or(WireShape::Unknown);
                    self.fact.state(status, body);
                }
            }
            Expr::Path(path) => {
                let segments: Vec<String> = path
                    .path
                    .segments
                    .iter()
                    .map(|segment| segment.ident.to_string())
                    .collect();
                if let ["StatusCode", constant] = segments
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>()
                    .as_slice()
                {
                    if let Some(status) = named_status(constant) {
                        self.status_literals.push(status);
                    }
                }
            }
            _ => {}
        }
        syn::visit::visit_expr(self, expr);
    }
}

/// The status of an `HttpResponse::Ok()`-style builder call.
fn http_response_status(receiver: &Expr) -> Option<u16> {
    let Expr::Call(call) = unwrap_paren(receiver) else {
        return None;
    };
    let Expr::Path(path) = &*call.func else {
        return None;
    };
    let mut segments = path.path.segments.iter().rev();
    let constructor = segments.next()?.ident.to_string();
    if segments.next()?.ident != "HttpResponse" {
        return None;
    }
    named_status(&constructor)
}

/// The wire shape of a serialized value expression: a struct literal states
/// its type, an identifier states its binding's, a `Json(x)` wrapper states
/// its payload's. Everything else is unknown, not guessed.
fn value_shape(expr: &Expr, locals: &BTreeMap<String, WireShape>) -> WireShape {
    match unwrap_paren(expr) {
        Expr::Struct(item) => item
            .path
            .segments
            .last()
            .map(|segment| WireShape::Named(segment.ident.to_string()))
            .unwrap_or(WireShape::Unknown),
        Expr::Call(call) => {
            let is_json = matches!(&*call.func, Expr::Path(path)
                if path.path.segments.last().is_some_and(|s| s.ident == "Json"));
            match (is_json, call.args.first()) {
                (true, Some(inner)) => value_shape(inner, locals),
                _ => WireShape::Unknown,
            }
        }
        Expr::Path(path) => match path.path.get_ident() {
            Some(ident) => locals
                .get(&ident.to_string())
                .cloned()
                .unwrap_or(WireShape::Unknown),
            None => WireShape::Unknown,
        },
        Expr::Lit(lit) => match &lit.lit {
            Lit::Str(_) => WireShape::Primitive("string"),
            Lit::Int(_) => WireShape::Primitive("integer"),
            Lit::Float(_) => WireShape::Primitive("number"),
            Lit::Bool(_) => WireShape::Primitive("boolean"),
            _ => WireShape::Unknown,
        },
        _ => WireShape::Unknown,
    }
}

/// A unit-only enum's serde-visible values. A data-carrying variant means the
/// set is not closed, so it abstains.
pub(super) fn unit_variants(item: &syn::ItemEnum) -> Option<Vec<String>> {
    let rename_all = rename_all(&item.attrs);
    let mut values = Vec::new();
    for variant in &item.variants {
        if !matches!(variant.fields, syn::Fields::Unit) {
            return None;
        }
        values.push(match serde_rename(&variant.attrs) {
            Some(renamed) => renamed,
            None => super::field_facts::apply_rename_all(
                &variant.ident.to_string(),
                rename_all.as_deref(),
            ),
        });
    }
    (!values.is_empty()).then_some(values)
}

/// A container's `rename_all` rule, if it declares one.
fn rename_all(attrs: &[syn::Attribute]) -> Option<String> {
    attrs.iter().find_map(|attr| {
        let text = quote_attr(attr);
        if !text.contains("serde") {
            return None;
        }
        text.split("rename_all")
            .nth(1)?
            .split('"')
            .nth(1)
            .map(str::to_string)
    })
}

/// Whether a serde attribute list carries a bare flag, in either the
/// `#[serde(default)]` or the `#[serde(default = "path")]` spelling.
///
/// The token stream is spaced out by the parser, so matching is done on the
/// separated words rather than on the source text.
fn serde_flag(attrs: &[syn::Attribute], flag: &str) -> bool {
    attrs.iter().any(|attr| {
        let text = quote_attr(attr);
        if !text.contains("serde") {
            return false;
        }
        text.split(|c: char| !c.is_alphanumeric() && c != '_')
            .any(|word| word == flag)
    })
}

pub(super) fn serde_rename(attrs: &[syn::Attribute]) -> Option<String> {
    attrs.iter().find_map(|attr| {
        let text = quote_attr(attr);
        if !text.contains("serde") {
            return None;
        }
        let after = text.split("rename").nth(1)?;
        if after.trim_start().starts_with("_all") {
            return None;
        }
        after.split('"').nth(1).map(str::to_string)
    })
}

/// An attribute's argument text. serde and validator take arbitrary token
/// trees, so this last mile stays textual, but it operates on tokens the parser
/// produced rather than on a line of a file.
pub(super) fn quote_attr(attr: &syn::Attribute) -> String {
    let path = attr
        .path()
        .segments
        .last()
        .map(|segment| segment.ident.to_string())
        .unwrap_or_default();
    match &attr.meta {
        syn::Meta::List(list) => format!("{path} {}", list.tokens),
        _ => path,
    }
}

/// Value guards found in a handler body, by the field they constrain.
///
/// A Rust type carries no value range: `rating: i8` says nothing, and the
/// constraint that actually rejects the request is two lines into the handler.
/// Over a parse these are exact expressions rather than a line that looked
/// right, so `matches!(body.rating, -1 | 0 | 1)` is read as the closed set it
/// is, and a guard whose alternatives are not literals is left alone.
#[derive(Default)]
pub(super) struct Guards {
    /// field -> the values an explicit guard accepts.
    pub(super) allowed: BTreeMap<String, Vec<String>>,
    /// field -> the bounds an explicit range guard accepts.
    pub(super) ranges: BTreeMap<String, (Option<f64>, Option<f64>)>,
}

impl<'ast> syn::visit::Visit<'ast> for Guards {
    fn visit_expr(&mut self, expr: &'ast Expr) {
        match expr {
            // `matches!(body.rating, -1 | 0 | 1)`
            Expr::Macro(node) if node.mac.path.is_ident("matches") => {
                let tokens = node.mac.tokens.to_string();
                if let Some((scrutinee, arms)) = tokens.split_once(',') {
                    if let (Some(field), Some(values)) =
                        (mentioned_field(scrutinee), literal_list(arms, '|'))
                    {
                        self.allowed.entry(field).or_insert(values);
                    }
                }
            }
            // `[-1, 0, 1].contains(&body.rating)` and `(1..=5).contains(..)`
            Expr::MethodCall(call) if call.method == "contains" => {
                let Some(field) = call
                    .args
                    .first()
                    .and_then(|arg| mentioned_field(&expr_text(arg)))
                else {
                    syn::visit::visit_expr(self, expr);
                    return;
                };
                match unwrap_paren(&call.receiver) {
                    Expr::Array(array) => {
                        let items = array
                            .elems
                            .iter()
                            .map(expr_text)
                            .collect::<Vec<_>>()
                            .join(",");
                        if let Some(values) = literal_list(&items, ',') {
                            self.allowed.entry(field).or_insert(values);
                        }
                    }
                    Expr::Range(range) => {
                        let low = range.start.as_ref().and_then(|e| numeric(e));
                        let inclusive = matches!(range.limits, syn::RangeLimits::Closed(_));
                        let high = range.end.as_ref().and_then(|e| numeric(e)).map(|high| {
                            if inclusive {
                                high
                            } else {
                                high - 1.0
                            }
                        });
                        if low.is_some() || high.is_some() {
                            self.ranges.entry(field).or_insert((low, high));
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        syn::visit::visit_expr(self, expr);
    }
}

pub(super) fn unwrap_paren(expr: &Expr) -> &Expr {
    match expr {
        Expr::Paren(inner) => unwrap_paren(&inner.expr),
        Expr::Reference(inner) => unwrap_paren(&inner.expr),
        other => other,
    }
}

/// The last identifier of a field access, which is the field a guard is about.
pub(super) fn mentioned_field(text: &str) -> Option<String> {
    let cleaned: String = text
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '_' || *c == '.')
        .collect();
    let last = cleaned.rsplit('.').next()?.trim().to_string();
    (!last.is_empty() && last.chars().next()?.is_alphabetic()).then_some(last)
}

pub(super) fn expr_text(expr: &Expr) -> String {
    match expr {
        Expr::Lit(lit) => match &lit.lit {
            Lit::Str(text) => format!("\"{}\"", text.value()),
            Lit::Int(value) => value.to_string(),
            Lit::Float(value) => value.to_string(),
            other => format!("{other:?}"),
        },
        Expr::Unary(unary) => format!("-{}", expr_text(&unary.expr)),
        Expr::Reference(inner) => expr_text(&inner.expr),
        Expr::Field(field) => match &field.member {
            syn::Member::Named(name) => format!(".{name}"),
            syn::Member::Unnamed(_) => String::new(),
        },
        Expr::MethodCall(call) => expr_text(&call.receiver),
        Expr::Path(path) => path
            .path
            .segments
            .last()
            .map(|segment| segment.ident.to_string())
            .unwrap_or_default(),
        _ => String::new(),
    }
}

pub(super) fn numeric(expr: &Expr) -> Option<f64> {
    expr_text(expr).replace(' ', "").parse().ok()
}

/// The literal items of a separated list, or None if any item is computed: a
/// list with one non-literal element states no closed set.
pub(super) fn literal_list(text: &str, separator: char) -> Option<Vec<String>> {
    let mut values = Vec::new();
    for part in text.split(separator) {
        let item = part.trim().replace(' ', "");
        if item.is_empty() || item == "_" {
            return None;
        }
        let unquoted = item.trim_matches('"').to_string();
        if unquoted == item && item.parse::<f64>().is_err() {
            return None;
        }
        values.push(unquoted);
    }
    (values.len() > 1).then_some(values)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflows::backend_learn::rust_ast;

    fn read_source(case: &str, files: &[(&str, &str)]) -> rust_ast::RustSource {
        let root =
            std::env::temp_dir().join(format!("reproit-rtypes-{}-{case}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("root");
        for (name, body) in files {
            std::fs::write(root.join(name), body).expect("write");
        }
        let source = rust_ast::read(&root);
        let _ = std::fs::remove_dir_all(&root);
        source
    }

    #[test]
    fn a_json_return_type_states_a_200_with_its_payload_shape() {
        let source = read_source(
            "json_return",
            &[(
                "main.rs",
                r#"
            pub struct Item { pub id: String, pub price: f64 }
            pub async fn list() -> Json<Vec<Item>> { todo!() }
            pub async fn show() -> Result<Json<Item>, AppError> { todo!() }
            fn app() -> Router {
                Router::new().route("/items", get(list)).route("/items/{id}", get(show))
            }
            "#,
            )],
        );
        let list = source.responses.get("list").expect("stated");
        assert_eq!(
            list.statuses[&200],
            WireShape::Array(Box::new(WireShape::Named("Item".into())))
        );
        let show = source.responses.get("show").expect("stated");
        assert_eq!(
            show.statuses[&200],
            WireShape::Named("Item".into()),
            "only the Ok arm of a Result speaks"
        );
        assert_eq!(show.statuses.len(), 1, "the error arm states no status");
        let item = source.serializers.get("Item").expect("collected");
        assert_eq!(item["id"].shape, WireShape::Primitive("string"));
        assert_eq!(item["price"].shape, WireShape::Primitive("number"));
        assert!(item["id"].required);
    }

    #[test]
    fn a_tuple_return_carries_the_payload_at_each_named_status() {
        let source = read_source(
            "tuple_return",
            &[(
                "main.rs",
                r#"
            pub struct Made { pub id: String }
            pub async fn create() -> (StatusCode, Json<Made>) {
                (StatusCode::CREATED, Json(Made { id: "x".into() }))
            }
            fn app() -> Router { Router::new().route("/items", post(create)) }
            "#,
            )],
        );
        let create = source.responses.get("create").expect("stated");
        assert_eq!(create.statuses[&201], WireShape::Named("Made".into()));
        assert!(
            !create.statuses.contains_key(&200),
            "a tuple return has no default status: {:?}",
            create.statuses
        );
    }

    #[test]
    fn a_tuple_return_naming_no_status_literal_abstains() {
        let source = read_source(
            "tuple_opaque",
            &[(
                "main.rs",
                r#"
            pub struct Made { pub id: String }
            pub async fn create(state: State<App>) -> (StatusCode, Json<Made>) {
                (state.status_for(), Json(Made { id: "x".into() }))
            }
            fn app() -> Router { Router::new().route("/items", post(create)) }
            "#,
            )],
        );
        assert!(
            !source.responses.contains_key("create"),
            "a computed status is not a stated one: {:?}",
            source.responses
        );
    }

    #[test]
    fn an_actix_builder_states_status_and_body_in_one_expression() {
        let source = read_source(
            "actix_builder",
            &[(
                "main.rs",
                r#"
            pub struct Widget { pub name: String }
            pub async fn create() -> impl Responder {
                let made = Widget { name: "w".into() };
                HttpResponse::Created().json(made)
            }
            pub async fn missing() -> impl Responder {
                HttpResponse::NotFound().json("gone")
            }
            "#,
            )],
        );
        let create = source.responses.get("create").expect("stated");
        assert_eq!(
            create.statuses[&201],
            WireShape::Named("Widget".into()),
            "the local's struct literal types the json call"
        );
        let missing = source.responses.get("missing").expect("stated");
        assert_eq!(missing.statuses[&404], WireShape::Primitive("string"));
    }

    #[test]
    fn serde_serialization_attributes_shape_the_wire_fields() {
        let source = read_source(
            "serde_wire",
            &[(
                "main.rs",
                r#"
            #[serde(rename_all = "camelCase")]
            pub struct Out {
                pub item_count: u32,
                #[serde(skip_serializing_if = "Option::is_none")]
                pub note: Option<String>,
                pub always_null: Option<String>,
                #[serde(skip)]
                pub secret: String,
                #[serde(serialize_with = "compact")]
                pub rewritten: u32,
            }
            pub async fn stats() -> Json<Out> { todo!() }
            "#,
            )],
        );
        let out = source.serializers.get("Out").expect("collected");
        assert_eq!(out["itemCount"].shape, WireShape::Primitive("integer"));
        assert!(out["itemCount"].required);
        assert!(!out["note"].required, "skip_serializing_if may omit it");
        assert_eq!(
            out["note"].shape,
            WireShape::Primitive("string"),
            "when present it carries the inner type"
        );
        assert!(
            out["alwaysNull"].required,
            "a bare Option is always present"
        );
        assert_eq!(
            out["alwaysNull"].shape,
            WireShape::Unknown,
            "and claims no type, because None is null"
        );
        assert!(!out.contains_key("secret"), "skip is never on the wire");
        assert_eq!(
            out["rewritten"].shape,
            WireShape::Unknown,
            "a custom serializer means the type no longer states the shape"
        );
    }

    #[test]
    fn a_flattened_serializer_and_a_conflicting_name_both_abstain() {
        let source = read_source(
            "wire_abstain",
            &[
                (
                    "a.rs",
                    "pub struct Flat { pub a: String, #[serde(flatten)] pub extra: Meta }\n\
                     pub struct Twin { pub a: String }\n",
                ),
                ("b.rs", "pub struct Twin { pub b: u32 }\n"),
            ],
        );
        assert!(
            !source.serializers.contains_key("Flat"),
            "an unenumerable shape must abstain: {:?}",
            source.serializers.keys()
        );
        assert!(
            !source.serializers.contains_key("Twin"),
            "two different declarations resolve to neither: {:?}",
            source.serializers.keys()
        );
    }
}
