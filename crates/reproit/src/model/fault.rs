//! Spectrum-based fault localization (instrument v1b).
//!
//! Given coverage from several runs, each labeled pass/fail, rank code elements
//! by how suspicious they are of causing the failures. We use Ochiai, the
//! standard SBFL metric (Abreu et al.): an element executed mostly on failing
//! runs and rarely on passing ones scores high.
//!
//!   susp(e) = ef / sqrt(totalFailed * (ef + ep))
//!
//! where ef = #failing runs that cover e, ep = #passing runs that cover e.
//! Elements come from `vmservice::collect_coverage` as "<script-uri>#<pos>"
//! tokens, but the metric is element-agnostic, any consistent identifier works.

use std::collections::BTreeSet;

/// One run's contribution: did it pass, and which elements did it cover.
pub struct RunCoverage {
    pub passed: bool,
    pub covered: BTreeSet<String>,
}

/// Ochiai suspiciousness per element, highest first. Elements covered by zero
/// failing runs are dropped (suspiciousness 0). Ties broken by element id for
/// determinism.
pub fn ochiai(runs: &[RunCoverage]) -> Vec<(String, f64)> {
    let total_failed = runs.iter().filter(|r| !r.passed).count() as f64;
    if total_failed == 0.0 {
        return Vec::new(); // nothing failed -> nothing to localize
    }
    let mut elements: BTreeSet<&String> = BTreeSet::new();
    for r in runs {
        elements.extend(r.covered.iter());
    }
    let mut scored: Vec<(String, f64)> = elements
        .into_iter()
        .filter_map(|e| {
            let mut ef = 0.0;
            let mut ep = 0.0;
            for r in runs {
                if r.covered.contains(e) {
                    if r.passed {
                        ep += 1.0;
                    } else {
                        ef += 1.0;
                    }
                }
            }
            if ef == 0.0 {
                return None;
            }
            let denom = (total_failed * (ef + ep)).sqrt();
            Some((e.clone(), if denom > 0.0 { ef / denom } else { 0.0 }))
        })
        .collect();
    // sort by suspiciousness desc, then element id asc for stable output.
    scored.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.0.cmp(&b.0))
    });
    scored
}

#[cfg(test)]
mod tests {
    use super::*;

    // Property test (Hegel): for ANY set of pass/fail runs, the Ochiai output
    // must be well-formed, scores in [0,1], sorted descending, deterministic,
    // and never listing an element that no failing run covered.
    #[hegel::test]
    fn ochiai_output_is_wellformed(tc: hegel::TestCase) {
        let n: usize = tc.draw(
            hegel::generators::integers::<usize>()
                .min_value(1)
                .max_value(8),
        );
        let mut runs = Vec::new();
        for _ in 0..n {
            let passed = tc.draw(hegel::generators::booleans());
            // coverage elements drawn from a small universe so runs overlap.
            let covered: Vec<String> = tc.draw(
                hegel::generators::vecs(
                    hegel::generators::text()
                        .alphabet("abcde")
                        .min_size(1)
                        .max_size(1),
                )
                .max_size(5),
            );
            runs.push(RunCoverage {
                passed,
                covered: covered.into_iter().collect(),
            });
        }
        let ranked = ochiai(&runs);
        for (e, s) in &ranked {
            assert!(
                (0.0..=1.0 + 1e-9).contains(s),
                "score {s} out of [0,1] for {e}"
            );
            let covers_fail = runs.iter().any(|r| !r.passed && r.covered.contains(e));
            assert!(covers_fail, "{e} ranked but no failing run covers it");
        }
        for w in ranked.windows(2) {
            assert!(w[0].1 >= w[1].1, "ochiai output not sorted descending");
        }
        assert_eq!(ranked, ochiai(&runs), "ochiai must be deterministic");
    }

    fn cov(passed: bool, elems: &[&str]) -> RunCoverage {
        RunCoverage {
            passed,
            covered: elems.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn the_uniquely_failing_element_ranks_first() {
        // "bug" runs only in failing runs; "common" runs everywhere; "safe"
        // only in passing runs. Ochiai must surface "bug" at the top.
        let runs = vec![
            cov(true, &["common", "safe"]),
            cov(true, &["common", "safe"]),
            cov(false, &["common", "bug"]),
            cov(false, &["common", "bug"]),
        ];
        let ranked = ochiai(&runs);
        assert_eq!(ranked.first().map(|(e, _)| e.as_str()), Some("bug"));
        // "safe" never appears in a failing run -> excluded entirely.
        assert!(ranked.iter().all(|(e, _)| e != "safe"));
        // a perfectly-correlated fault has suspiciousness 1.0.
        assert!((ranked[0].1 - 1.0).abs() < 1e-9);
    }

    #[test]
    fn no_failures_means_nothing_to_localize() {
        let runs = vec![cov(true, &["a"]), cov(true, &["b"])];
        assert!(ochiai(&runs).is_empty());
    }

    #[test]
    fn output_is_deterministic() {
        let runs = vec![
            cov(false, &["x", "y"]),
            cov(true, &["x"]),
            cov(false, &["x", "y"]),
        ];
        assert_eq!(ochiai(&runs), ochiai(&runs));
    }
}
