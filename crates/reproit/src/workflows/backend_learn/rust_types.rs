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
    let mut fields = BTreeMap::new();
    for field in &named.named {
        let Some(ident) = &field.ident else { continue };
        let name = serde_rename(&field.attrs).unwrap_or_else(|| ident.to_string());
        let optional = inner_of(&field.ty, "Option").is_some();
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

/// A unit-only enum's serde-visible values. A data-carrying variant means the
/// set is not closed, so it abstains.
pub(super) fn unit_variants(item: &syn::ItemEnum) -> Option<Vec<String>> {
    let rename_all = item.attrs.iter().find_map(|attr| {
        let text = quote_attr(attr);
        text.split("rename_all")
            .nth(1)?
            .split('"')
            .nth(1)
            .map(str::to_string)
    });
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
