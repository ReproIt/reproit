//! The parse harness every non-Rust family shares.
//!
//! Each family differs only in what its tree MEANS. Opening files, running the
//! grammar, counting what would not parse, and walking the tree are the same
//! job seven times, so they live here once and the family readers hold nothing
//! but their own vocabulary.
//!
//! The counting is the point. A pattern reader cannot distinguish a construct
//! it failed to match from one that is not there, and every false "the source
//! does not serve this operation" came from that. A grammar can: a file either
//! parses, in which case what it declares was read exactly, or it does not, in
//! which case it is counted and the caller must qualify its own conclusions.

use super::extract::Family;
use super::field_facts::FieldFact;
use std::collections::BTreeMap;
use std::path::Path;
use tree_sitter::{Node, Parser};

/// Bound the fields one declaration may contribute.
pub(super) const MAX_FIELDS: usize = 512;

/// What one family's sources declare. Every reader returns this, so wiring a
/// family into the extractor is the same three lines regardless of language.
#[derive(Debug, Default)]
pub(super) struct SourceRead {
    /// Local path, method, and the handler serving it.
    pub(super) routes: Vec<(String, &'static str, Option<String>)>,
    /// handler -> the request body fields it accepts.
    pub(super) bodies: BTreeMap<String, BTreeMap<String, FieldFact>>,
    pub(super) files_parsed: usize,
    /// Files the grammar could not read. Non-zero means the reader has a blind
    /// spot, and any absence over these sources is not evidence of anything.
    pub(super) files_unreadable: usize,
}

/// Parse every source of a family, handing each clean tree to `visit`.
///
/// A file that does not parse is counted and NOT visited: half-reading a broken
/// file yields routes that are worse than none, because they look complete.
pub(super) fn read_files(
    root: &Path,
    family: Family,
    language: tree_sitter::Language,
    source: &mut SourceRead,
    visit: impl FnMut(Node, &str, &Path),
) {
    read_files_with(root, family, |_| Some(language.clone()), source, visit);
}

/// The same, choosing a grammar per file.
///
/// One family can span dialects: `.ts` is not `.js`. Parsing TypeScript with
/// the JavaScript grammar makes every annotated file an error, and since a file
/// that does not parse is counted rather than read, a whole TypeScript service
/// came back as zero routes.
pub(super) fn read_files_with(
    root: &Path,
    family: Family,
    language_for: impl Fn(&Path) -> Option<tree_sitter::Language>,
    source: &mut SourceRead,
    mut visit: impl FnMut(Node, &str, &Path),
) {
    let mut parser = Parser::new();
    let mut current: Option<tree_sitter::Language> = None;
    for file in super::extract::family_sources(root, family) {
        let Some(language) = language_for(&file) else {
            continue;
        };
        if current.as_ref() != Some(&language) {
            if parser.set_language(&language).is_err() {
                continue;
            }
            current = Some(language);
        }
        // A file that cannot be decoded or opened is a file the reader did not
        // read. Skipping it silently made a permission error and a byte the
        // decoder rejected look exactly like a file that declares nothing.
        let Ok(text) = std::fs::read_to_string(&file) else {
            source.files_unreadable += 1;
            continue;
        };
        match parser.parse(&text, None) {
            Some(tree) if !tree.root_node().has_error() => {
                source.files_parsed += 1;
                visit(tree.root_node(), &text, &file);
            }
            _ => source.files_unreadable += 1,
        }
    }
}

/// Preorder walk, calling `visit` on every node including the root.
pub(super) fn walk(node: Node, visit: &mut impl FnMut(Node)) {
    visit(node);
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk(child, visit);
    }
}

/// A node's source text.
pub(super) fn text<'a>(node: Node, source: &'a str) -> &'a str {
    node.utf8_text(source.as_bytes()).unwrap_or("")
}

/// A node's named children, in order.
pub(super) fn children<'a>(node: Node<'a>, into: &mut Vec<Node<'a>>) {
    into.clear();
    let mut cursor = node.walk();
    into.extend(node.children(&mut cursor).filter(Node::is_named));
}

/// The text of a field, if the node has one.
pub(super) fn field(node: Node, source: &str, name: &str) -> Option<String> {
    node.child_by_field_name(name)
        .map(|child| text(child, source).to_string())
}

/// A string literal's content, with the quoting of any of these languages
/// stripped. Only the outer quotes are removed, so an inner apostrophe stays.
pub(super) fn unquote(raw: &str) -> &str {
    let raw = raw.trim();
    for quote in ['"', '\'', '`'] {
        if let Some(inner) = raw.strip_prefix(quote).and_then(|r| r.strip_suffix(quote)) {
            return inner;
        }
    }
    raw
}

/// The first descendant of a kind, or None.
pub(super) fn find<'a>(node: Node<'a>, kind: &str) -> Option<Node<'a>> {
    if node.kind() == kind {
        return Some(node);
    }
    let mut cursor = node.walk();
    let children: Vec<Node<'a>> = node.children(&mut cursor).collect();
    children.into_iter().find_map(|child| find(child, kind))
}

/// Parse a numeric literal, tolerating the unary minus that every one of these
/// grammars puts in its own node.
pub(super) fn number(raw: &str) -> Option<f64> {
    let compact: String = raw.chars().filter(|c| !c.is_whitespace()).collect();
    compact.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_outer_quotes_come_off() {
        assert_eq!(unquote("'/v1/blocks'"), "/v1/blocks");
        assert_eq!(unquote("\"/v1/blocks\""), "/v1/blocks");
        assert_eq!(unquote("`/v1/blocks`"), "/v1/blocks");
        assert_eq!(unquote("it's"), "it's");
        assert_eq!(unquote("'it''s'"), "it''s");
    }

    #[test]
    fn a_negative_literal_parses_after_the_grammar_splits_it() {
        assert_eq!(number("- 1"), Some(-1.0));
        assert_eq!(number("1"), Some(1.0));
        assert_eq!(number("user"), None);
    }
}
