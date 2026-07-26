//! Parsed source for the families that are not Rust.
//!
//! Rust reads through `syn`, and the value of that was never that a parser
//! matches more: it is that a parser KNOWS WHEN IT FAILED. A pattern cannot.
//! Every false "the source does not serve this operation" came from an
//! unreadable construct being indistinguishable from an absent one.
//!
//! The other five families keep their pattern extractors, which are good at
//! what they do, but each file is now also parsed. A file the grammar cannot
//! read is COUNTED, and the drift check downgrades its own conclusions
//! accordingly instead of reporting an absence it has no standing to report.
//!
//! One grammar per language and one code path, so adding the next family is a
//! table row rather than a second parser integration.

use super::extract::Family;
use std::path::Path;
use tree_sitter::{Parser, Tree};

/// What a parse of one family's sources found.
#[derive(Debug, Default)]
pub(super) struct ParseReport {
    pub(super) files_parsed: usize,
    /// Files whose grammar reported an error. While this is non-zero the
    /// reader has a blind spot, and any absence over these sources is not
    /// evidence of anything.
    pub(super) files_unreadable: usize,
}

/// The grammar for a family, or None where the extractor is still the only
/// reader. Returning None is honest: it means no parse was attempted, which is
/// different from a parse that found nothing.
fn language(family: Family) -> Option<tree_sitter::Language> {
    Some(match family {
        Family::Python => tree_sitter_python::LANGUAGE.into(),
        Family::Node => tree_sitter_javascript::LANGUAGE.into(),
        Family::Ruby => tree_sitter_ruby::LANGUAGE.into(),
        Family::Php => tree_sitter_php::LANGUAGE_PHP.into(),
        Family::Spring => tree_sitter_java::LANGUAGE.into(),
        // Rust is read by `syn`, which is a full parse rather than a grammar.
        Family::Rust | Family::Go => return None,
    })
}

/// Parse every source of a family, reporting what could not be read.
pub(super) fn check(root: &Path, family: Family) -> ParseReport {
    let mut report = ParseReport::default();
    let Some(language) = language(family) else {
        return report;
    };
    let mut parser = Parser::new();
    if parser.set_language(&language).is_err() {
        return report;
    }
    for file in super::extract::family_sources(root, family) {
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        match parser.parse(&text, None) {
            Some(tree) if !has_error(&tree) => report.files_parsed += 1,
            // Both a grammar error and a refusal to produce a tree mean the
            // same thing: this file was not read.
            _ => report.files_unreadable += 1,
        }
    }
    report
}

/// Whether the grammar reported an error anywhere in the tree.
///
/// tree-sitter always returns a tree, inserting ERROR nodes where it could not
/// make sense of the input, so the root flag is the only reliable signal that
/// the file was actually understood.
fn has_error(tree: &Tree) -> bool {
    tree.root_node().has_error()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root(case: &str, files: &[(&str, &str)]) -> std::path::PathBuf {
        let root =
            std::env::temp_dir().join(format!("reproit-parsed-{}-{case}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("root");
        for (name, body) in files {
            std::fs::write(root.join(name), body).expect("write");
        }
        root
    }

    #[test]
    fn probe_python_shape() {
        let src = r#"
@app.post("/v1/blocks")
async def create_block(body: BlockRequest):
    return {}
"#;
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_python::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(src, None).unwrap();
        fn walk(n: tree_sitter::Node, src: &str, d: usize) {
            if d < 6 {
                eprintln!(
                    "{}{} :: {}",
                    "  ".repeat(d),
                    n.kind(),
                    n.utf8_text(src.as_bytes())
                        .unwrap_or("")
                        .lines()
                        .next()
                        .unwrap_or("")
                );
            }
            let mut c = n.walk();
            for ch in n.children(&mut c) {
                walk(ch, src, d + 1);
            }
        }
        walk(tree.root_node(), src, 0);
    }

    #[test]
    fn valid_python_parses_and_broken_python_is_counted() {
        let dir = root(
            "python",
            &[
                ("good.py", "def handler(body):\n    return {}\n"),
                ("bad.py", "def broken(:\n  ???\n"),
            ],
        );
        let report = check(&dir, Family::Python);
        assert_eq!(report.files_parsed, 1);
        assert_eq!(report.files_unreadable, 1, "an unread file must be counted");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn every_grammar_backed_family_reads_its_own_language() {
        for (case, family, name, body) in [
            ("py", Family::Python, "a.py", "x = 1\n"),
            ("js", Family::Node, "a.js", "const x = 1;\n"),
            ("rb", Family::Ruby, "a.rb", "x = 1\n"),
            ("php", Family::Php, "a.php", "<?php $x = 1;\n"),
            ("java", Family::Spring, "A.java", "class A { int x; }\n"),
        ] {
            let dir = root(case, &[(name, body)]);
            let report = check(&dir, family);
            assert_eq!(
                (report.files_parsed, report.files_unreadable),
                (1, 0),
                "{name} should parse cleanly"
            );
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn a_family_with_no_grammar_reports_no_parse_rather_than_a_clean_one() {
        // Go still has only its pattern reader. Claiming zero unreadable files
        // would imply a parse that never happened.
        let dir = root("go", &[("a.go", "package main\n")]);
        let report = check(&dir, Family::Go);
        assert_eq!((report.files_parsed, report.files_unreadable), (0, 0));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_truncated_file_of_each_language_is_caught() {
        for (case, family, name, body) in [
            ("py-bad", Family::Python, "a.py", "def f(:\n"),
            ("js-bad", Family::Node, "a.js", "function f( { \n"),
            ("rb-bad", Family::Ruby, "a.rb", "def f(\n"),
            (
                "java-bad",
                Family::Spring,
                "A.java",
                "class A { void f( {\n",
            ),
        ] {
            let dir = root(case, &[(name, body)]);
            let report = check(&dir, family);
            assert_eq!(
                report.files_unreadable, 1,
                "{name} should not read as clean"
            );
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}
