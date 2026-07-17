use serde_json::Value;
use std::collections::BTreeSet;

/// Parse a `--locale de,ar,ja` list into a deduped, order-preserving vector of
/// trimmed, non-empty locale tags. An empty / all-blank input yields an empty
/// vector, which the caller treats as "app default, behavior unchanged".
pub fn parse_locales(raw: &str) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for tok in raw.split(',') {
        let loc = tok.trim();
        if loc.is_empty() {
            continue;
        }
        if seen.insert(loc.to_string()) {
            out.push(loc.to_string());
        }
    }
    out
}

/// The dart-define / env var name that carries the locale to every runner.
pub const LOCALE_ENV: &str = "REPROIT_LOCALE";

/// Tag a finding with the locale it was found under (in place). Pure given the
/// value; the caller owns the locale-loop.
pub fn tag_finding_locale(finding: &mut Value, locale: &str) {
    if let Some(obj) = finding.as_object_mut() {
        obj.insert("locale".to_string(), Value::String(locale.to_string()));
    }
}

/// Given per-locale finding signatures (locale -> set of finding signatures),
/// return the signatures that appear in SOME locale but not ALL of them. These
/// are locale-specific i18n findings (e.g. an overflow only in `de`). When
/// fewer than two locales ran, there is nothing to compare, so the result is
/// empty.
pub fn locale_specific_findings(
    per_locale: &[(String, BTreeSet<String>)],
) -> Vec<(String, Vec<String>)> {
    if per_locale.len() < 2 {
        return Vec::new();
    }
    // Union of all signatures across locales.
    let mut all: BTreeSet<String> = BTreeSet::new();
    for (_, sigs) in per_locale {
        all.extend(sigs.iter().cloned());
    }
    let mut out = Vec::new();
    for sig in all {
        let present: Vec<String> = per_locale
            .iter()
            .filter(|(_, sigs)| sigs.contains(&sig))
            .map(|(loc, _)| loc.clone())
            .collect();
        if present.len() < per_locale.len() {
            out.push((sig, present));
        }
    }
    out
}
