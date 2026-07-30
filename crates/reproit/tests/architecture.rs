//! Cheap architecture ratchets for boundaries that Rust does not encode itself.

use std::path::PathBuf;

fn source(relative: &str) -> String {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(manifest.join(relative))
        .unwrap_or_else(|error| panic!("read {relative}: {error}"))
}

#[test]
fn process_entry_point_stays_thin() {
    let main = source("src/main.rs");
    let code_lines = main
        .lines()
        .filter(|line| {
            let line = line.trim();
            !line.is_empty() && !line.starts_with("//!") && !line.starts_with("//")
        })
        .count();
    assert!(
        code_lines <= 10,
        "src/main.rs grew to {code_lines} code lines; put application logic in the library"
    );
    assert!(
        main.contains("reproit::startup()"),
        "src/main.rs must delegate to the bounded-stack startup path"
    );
    assert!(
        !main.contains("tokio::main"),
        "src/main.rs must not poll the CLI future on the platform entry stack"
    );
}

#[test]
fn crate_root_stays_declarative() {
    let root = source("src/lib.rs");
    let code_lines = root
        .lines()
        .filter(|line| {
            let line = line.trim();
            !line.is_empty() && !line.starts_with("//!") && !line.starts_with("//")
        })
        .count();
    assert!(
        code_lines <= 80,
        "src/lib.rs grew to {code_lines} code lines; put behavior in a named module"
    );
}

#[test]
fn crate_root_does_not_restore_compatibility_aliases() {
    let root = source("src/lib.rs");
    for namespace in [
        "backends", "commands", "crosscut", "infra", "model", "modes",
    ] {
        let alias = format!("pub(crate) use {namespace}::");
        assert!(
            !root.contains(&alias),
            "src/lib.rs reintroduced the `{namespace}` compatibility aliases; use the owning \
             namespace at call sites"
        );
    }
    assert!(
        !root.contains("pub mod cli;"),
        "the internal CLI parser and context must not become public API"
    );
}

#[test]
fn legacy_source_namespaces_do_not_return() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for relative in [
        "src/backends",
        "src/cli",
        "src/commands",
        "src/crosscut",
        "src/infra",
        "src/model",
        "src/modes",
        "scaffolds",
    ] {
        assert!(
            !manifest.join(relative).exists(),
            "legacy namespace {relative} returned; use the owning architectural layer"
        );
    }
}

#[test]
fn inner_layers_do_not_depend_on_outer_layers() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for (layer, forbidden) in [
        ("domain", &["crate::interface", "crate::workflows"][..]),
        ("adapters", &["crate::interface", "crate::workflows"][..]),
        ("interface", &["crate::workflows"][..]),
    ] {
        let mut pending = vec![manifest.join("src").join(layer)];
        while let Some(directory) = pending.pop() {
            for entry in std::fs::read_dir(&directory).expect("read layer directory") {
                let path = entry.expect("read layer entry").path();
                if path.is_dir() {
                    pending.push(path);
                    continue;
                }
                if path.extension().is_none_or(|extension| extension != "rs") {
                    continue;
                }
                let body = std::fs::read_to_string(&path).expect("read layer source");
                let production = if path.file_name().is_some_and(|name| name == "tests.rs") {
                    ""
                } else {
                    body.split("#[cfg(test)]").next().unwrap_or(&body)
                };
                for dependency in forbidden {
                    assert!(
                        !production.contains(dependency),
                        "{} depends outward through {dependency}",
                        path.display()
                    );
                }
            }
        }
    }
}

/// The domain layer must be decidable from its inputs: no wall clock, no
/// network, no environment. `inner_layers_do_not_depend_on_outer_layers` checks
/// module direction, which is the wrong axis for this: an LLM call reached the
/// domain through a sibling CRATE without touching `crate::workflows`, and a
/// wall-clock read is not a module path at all. This checks the effects
/// themselves.
#[test]
fn domain_code_stays_deterministic() {
    // The documented exception: capsule retention reads its tuning from the
    // environment and ages capsules by wall clock. Shrink this list; never
    // grow it without the same explicit rationale in the file itself.
    const ALLOWED: &[&str] = &["src/domain/capsule/mod.rs"];
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut pending = vec![manifest.join("src/domain")];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory).expect("read domain directory") {
            let path = entry.expect("read domain entry").path();
            if path.is_dir() {
                // A `tests/` directory is compiled only under #[cfg(test)].
                if path.file_name().is_some_and(|name| name == "tests") {
                    continue;
                }
                pending.push(path);
                continue;
            }
            if path.extension().is_none_or(|extension| extension != "rs")
                || path.file_name().is_some_and(|name| name == "tests.rs")
            {
                continue;
            }
            let relative = path
                .strip_prefix(&manifest)
                .expect("domain path under manifest")
                .to_string_lossy()
                .into_owned();
            if ALLOWED.contains(&relative.as_str()) {
                continue;
            }
            let body = std::fs::read_to_string(&path).expect("read domain source");
            let production = body
                .split("#[cfg(test)]\nmod tests")
                .next()
                .unwrap_or(&body);
            for (effect, why) in [
                ("llm::", "a remote model call is nondeterministic"),
                ("reqwest", "the domain must not open network connections"),
                ("std::env", "environment reads belong in adapters"),
                ("SystemTime::now", "inject the caller's clock instead"),
                ("Utc::now()", "inject the caller's clock instead"),
            ] {
                assert!(
                    !production.contains(effect),
                    "{relative} reaches for {effect}: {why}"
                );
            }
        }
    }
}

#[test]
fn domain_map_does_not_acquire_platform_runs() {
    let map = source("src/domain/map/mod.rs");
    for forbidden in ["adapters::orchestrator", "run_journey(", "RunOpts"] {
        assert!(
            !map.contains(forbidden),
            "domain/map acquires a platform run through {forbidden}; keep acquisition in workflows"
        );
    }
}

#[test]
fn source_tree_uses_real_module_hierarchy() {
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut pending = vec![src];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory).expect("read source directory") {
            let path = entry.expect("source entry").path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                let body = std::fs::read_to_string(&path).expect("read Rust source");
                assert!(
                    !body.contains("#[path ="),
                    "{} bypasses the module hierarchy with #[path]",
                    path.display()
                );
            }
        }
    }
}

#[test]
fn production_code_uses_canonical_artifact_layout() {
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut pending = vec![src];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory).expect("read source directory") {
            let path = entry.expect("source entry").path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                // A conventional `tests.rs` module is compiled only through its parent's
                // `#[cfg(test)] mod tests;` declaration, so none of its contents are production.
                if path.file_name().is_some_and(|name| name == "tests.rs") {
                    continue;
                }
                let body = std::fs::read_to_string(&path).expect("read Rust source");
                let production = body.split("#[cfg(test)]").next().unwrap_or(&body);
                for forbidden in [".reproit/findings", ".reproit/tools"] {
                    assert!(
                        !production.contains(forbidden),
                        "{} hard-codes {forbidden}; use runtime/project_layout.rs",
                        path.display()
                    );
                }
            }
        }
    }
}

#[test]
fn source_files_stay_reviewable() {
    const MAX_LINES: usize = 1_000;
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut pending = vec![src];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory).expect("read source directory") {
            let path = entry.expect("source entry").path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                let body = std::fs::read_to_string(&path).expect("read Rust source");
                let lines = body.lines().count();
                assert!(
                    lines <= MAX_LINES,
                    "{} has {lines} lines; split responsibilities before exceeding {MAX_LINES}",
                    path.display()
                );
            }
        }
    }
}

/// `use super::*;` in production code makes a "split" cosmetic: the files share
/// one namespace, so the pieces never grow real interfaces and the line-count
/// ratchet can be satisfied by sharding a module without decoupling anything.
/// The allowlist below is the debt as of when this ratchet landed. The goal is
/// to SHRINK it: when a listed file drops its glob import, remove it here. A
/// new file must import what it uses by name.
#[test]
fn new_modules_do_not_glob_import_their_parent() {
    const ALLOWED: &[&str] = &[
        "src/adapters/atspi/actions.rs",
        "src/adapters/atspi/capture.rs",
        "src/adapters/atspi/protocol.rs",
        "src/adapters/atspi/session.rs",
        "src/adapters/execution/runner/automatic.rs",
        "src/adapters/execution/runner/catalog.rs",
        "src/adapters/execution/runner/process.rs",
        "src/adapters/orchestrator/watch.rs",
        "src/adapters/tui/action.rs",
        "src/adapters/tui/interaction.rs",
        "src/adapters/tui/invariants.rs",
        "src/adapters/tui/scenario.rs",
        "src/adapters/tui/screen.rs",
        "src/adapters/tui/session.rs",
        "src/adapters/uia/capture.rs",
        "src/adapters/uia/scenario.rs",
        "src/domain/backend/evaluate/fleet.rs",
        "src/domain/backend/evaluate/idempotency.rs",
        "src/domain/backend/evaluate/invariants.rs",
        "src/domain/backend/evaluate/lifecycle.rs",
        "src/domain/backend/evaluate/mod.rs",
        "src/domain/backend/evaluate/pending.rs",
        "src/domain/backend/evaluate/proofs.rs",
        "src/domain/backend/evaluate/proofs/graphql.rs",
        "src/domain/capsule/matching.rs",
        "src/domain/capsule/redaction.rs",
        "src/workflows/auth.rs",
        "src/workflows/auth/config_edit.rs",
        "src/workflows/backend_headless/accept.rs",
        "src/workflows/backend_headless/artifacts.rs",
        "src/workflows/backend_headless/binding.rs",
        "src/workflows/backend_headless/capture_replay.rs",
        "src/workflows/backend_headless/chaining.rs",
        "src/workflows/backend_headless/coverage.rs",
        "src/workflows/backend_headless/generation.rs",
        "src/workflows/backend_headless/history.rs",
        "src/workflows/backend_headless/inspect.rs",
        "src/workflows/backend_headless/inspect_plan.rs",
        "src/workflows/backend_headless/inspect_report.rs",
        "src/workflows/backend_headless/replay.rs",
        "src/workflows/backend_headless/replay_command.rs",
        "src/workflows/backend_headless/request.rs",
        "src/workflows/backend_headless/reset.rs",
        "src/workflows/backend_headless/retraction.rs",
        "src/workflows/backend_headless/round_trip.rs",
        "src/workflows/backend_headless/schema.rs",
        "src/workflows/backend_headless/shrink.rs",
        "src/workflows/backend_headless/transport.rs",
        "src/workflows/backend_headless/types.rs",
        "src/workflows/backend_headless/verify.rs",
        "src/workflows/bundle/cloud.rs",
        "src/workflows/bundle/format.rs",
        "src/workflows/capture.rs",
        "src/workflows/cloud.rs",
        "src/workflows/device.rs",
        "src/workflows/fuzz/campaign.rs",
        "src/workflows/fuzz/confirmation.rs",
        "src/workflows/fuzz/findings.rs",
        "src/workflows/fuzz/log.rs",
        "src/workflows/fuzz/reporting.rs",
        "src/workflows/fuzz/scan.rs",
        "src/workflows/fuzz/scan/recording.rs",
        "src/workflows/journey/execution.rs",
        "src/workflows/journey/persistence.rs",
        "src/workflows/journey/planning.rs",
        "src/workflows/journey/replay.rs",
        "src/workflows/journey/spec.rs",
        "src/workflows/journey/verification.rs",
        "src/workflows/map.rs",
        "src/workflows/record.rs",
        "src/workflows/repro.rs",
        "src/workflows/triage/lifecycle.rs",
        "src/workflows/triage/presentation.rs",
        "src/workflows/triage/reproduction.rs",
        "src/workflows/triage/setup.rs",
        "src/workflows/triage/transport.rs",
    ];
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut pending = vec![manifest.join("src")];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory).expect("read source directory") {
            let path = entry.expect("source entry").path();
            if path.is_dir() {
                if path.file_name().is_some_and(|name| name == "tests") {
                    continue;
                }
                pending.push(path);
                continue;
            }
            if path.extension().is_none_or(|extension| extension != "rs")
                || path.file_name().is_some_and(|name| name == "tests.rs")
            {
                continue;
            }
            let relative = path
                .strip_prefix(&manifest)
                .expect("source path under manifest")
                .to_string_lossy()
                .into_owned();
            let body = std::fs::read_to_string(&path).expect("read Rust source");
            let production = body
                .split("#[cfg(test)]\nmod tests")
                .next()
                .unwrap_or(&body);
            let globs = production.contains("use super::*;");
            if ALLOWED.contains(&relative.as_str()) {
                assert!(
                    globs,
                    "{relative} no longer glob-imports its parent; remove it from the \
                     allowlist so it cannot regress"
                );
            } else {
                assert!(
                    !globs,
                    "{relative} glob-imports its parent with `use super::*;`; import what \
                     it uses by name so the module has a real interface"
                );
            }
        }
    }
}

#[test]
fn responsibility_heavy_modules_stay_split() {
    const MAX_LINES: usize = 1_200;
    for relative in [
        "src/domain/capsule/mod.rs",
        "src/adapters/config/mod.rs",
        "src/interface/mcp/mod.rs",
        "src/adapters/project_scaffold/mod.rs",
    ] {
        let body = source(relative);
        let lines = body.lines().count();
        assert!(
            lines <= MAX_LINES,
            "{relative} has {lines} lines; move the next responsibility into a named submodule"
        );
    }
    let commands = source("src/workflows/mod.rs");
    assert!(
        commands.lines().count() <= 1_000,
        "src/workflows/mod.rs must stay below 1,000 lines; move command workflows into named modules"
    );
    for relative in [
        "src/domain/capsule/crypto.rs",
        "src/domain/capsule/matching.rs",
        "src/domain/capsule/redaction.rs",
        "src/adapters/config/loader.rs",
        "src/interface/mcp/dispatch.rs",
        "src/workflows/backend_target.rs",
        "src/workflows/check.rs",
        "src/workflows/fuzz_command.rs",
        "src/workflows/proof.rs",
        "src/workflows/create_command.rs",
        "src/workflows/scan_command.rs",
        "src/adapters/tui/capture.rs",
        "src/adapters/tui/fuzz_config.rs",
        "src/adapters/tui/interaction.rs",
        "src/adapters/tui/scenario.rs",
        "src/adapters/tui/session.rs",
    ] {
        assert!(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join(relative)
                .is_file(),
            "missing responsibility module {relative}"
        );
    }
}

#[test]
fn flutter_explorer_scaffold_stays_modular() {
    const MAX_ENTRY_LINES: usize = 40;
    const MAX_MODULE_LINES: usize = 700;
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/scaffolds/flutter");

    for entry in [
        root.join("integration_test/journey_explore.dart"),
        root.join("test/fuzz_headless_test.dart"),
    ] {
        let body = std::fs::read_to_string(&entry).expect("read Flutter explorer entry");
        let lines = body.lines().count();
        assert!(
            lines <= MAX_ENTRY_LINES,
            "{} has {lines} lines; keep application wiring in the entry and behavior in modules",
            entry.display()
        );
        assert!(
            !body.contains("class FuzzCfg") && !body.contains("Snapshot snapshot"),
            "{} duplicates explorer behavior instead of importing the shared library",
            entry.display()
        );
    }

    let modules = root.join("integration_test/reproit_explorer");
    for responsibility in [
        "navigation.dart",
        "action_execution.dart",
        "settling.dart",
        "oracle_collection.dart",
    ] {
        assert!(
            modules.join(responsibility).is_file(),
            "Flutter explorer is missing its {responsibility} responsibility module"
        );
    }
    for entry in std::fs::read_dir(modules).expect("read Flutter explorer modules") {
        let path = entry.expect("read Flutter explorer module entry").path();
        if path.extension().is_none_or(|extension| extension != "dart") {
            continue;
        }
        let body = std::fs::read_to_string(&path).expect("read Flutter explorer module");
        let lines = body.lines().count();
        assert!(
            lines <= MAX_MODULE_LINES,
            "{} has {lines} lines; split the module before exceeding {MAX_MODULE_LINES}",
            path.display()
        );
    }
}

/// The runner JS is authored as modules under runners/source/ and shipped as
/// generated single-file bundles (build-runner-bundles.mjs; CI rebuilds and
/// diffs, so the bundles cannot drift from the sources). The authored modules
/// carry the same reviewability cap as owned Rust; the generated bundles are
/// exempt, exactly like target/ output.
#[test]
fn runner_source_modules_stay_reviewable() {
    const MAX_LINES: usize = 1_000;
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../runners/source");
    let mut seen = 0usize;
    let mut stack = vec![root];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(dir).expect("read runners/source") {
            let path = entry.expect("read runners/source entry").path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().is_none_or(|extension| extension != "mjs") {
                continue;
            }
            let body = std::fs::read_to_string(&path).expect("read runner source module");
            let lines = body.lines().count();
            assert!(
                lines <= MAX_LINES,
                "{} has {lines} lines; split the module before exceeding {MAX_LINES}",
                path.display()
            );
            seen += 1;
        }
    }
    assert!(
        seen >= 30,
        "expected the authored runner modules, found {seen} files"
    );
}

/// The Electron and Tauri runners share one host-side canonical-signature and
/// scenario-plumbing implementation (runners/source/shared/). A runner that
/// stops importing it has started growing a private copy of the signature
/// algorithm, which is exactly the divergence the golden-vector parity gate
/// exists to prevent; fail here first, with a name.
#[test]
fn native_runners_compose_the_shared_signature_core() {
    let runners = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../runners");
    for entry in ["source/electron/part-01.mjs", "source/tauri/part-01.mjs"] {
        let body = std::fs::read_to_string(runners.join(entry)).expect("read runner entry");
        for module in ["./shared/signature.mjs", "./shared/fuzz.mjs"] {
            assert!(
                body.contains(module),
                "{entry} no longer imports {module}; the runner is forking the shared core"
            );
        }
        for private_copy in ["function signatureOf", "function fnv1a", "const ROLES"] {
            assert!(
                !body.contains(private_copy),
                "{entry} declares a private `{private_copy}`; use the shared module"
            );
        }
    }
}

/// Every path that can fail to observe keeps a distinct not-observed state.
///
/// One bug shape has produced nearly every correctness defect in this tool: an
/// absence reported as a positive. A replay that could not authenticate read as
/// "fixed"; an operation that only ever 429'd read as "clean"; no baseline read
/// as "pass"; a type declared twice read as a verdict; a route a pattern did not
/// match read as "does not exist"; a schema never compared read as "compared and
/// agrees".
///
/// Each was fixed by giving the absence its own state and refusing to merge it
/// with the negative result. This pins those states in place, so removing one
/// fails here instead of quietly restoring the bug. It cannot prove the property
/// holds everywhere; it does keep the instances that were paid for.
#[test]
fn absence_never_merges_with_a_negative_result() {
    for (relative, state, why) in [
        (
            "src/workflows/backend_headless/replay.rs",
            "Inconclusive",
            "a replay that could not be evaluated is not a proven fix",
        ),
        (
            "src/workflows/backend_headless/retraction.rs",
            "Retracted",
            "a claim the project withdrew is not a defect that was fixed",
        ),
        (
            "src/workflows/backend_headless/coverage.rs",
            "evaluated",
            "an operation that was reached but never answered is not clean",
        ),
        (
            "src/workflows/backend_headless/history.rs",
            "first_run",
            "no baseline is not a clean comparison",
        ),
        (
            "src/workflows/backend_learn/drift.rs",
            "bodies_compared",
            "a body that was never compared must not read as agreeing",
        ),
        (
            "src/workflows/backend_learn/field_facts.rs",
            "drop_ambiguous",
            "an ambiguous type must abstain rather than pick a winner",
        ),
        (
            "src/workflows/backend_learn/grammar.rs",
            "files_unreadable",
            "the shared parse harness must count what it could not read; every \
             family reader's honesty about absences rests on this one counter",
        ),
        (
            "src/workflows/backend_learn/rust_ast.rs",
            "files_unparsed",
            "a source file that could not be read is a known blind spot, not an \
             empty one, and an absence reported over it is not evidence",
        ),
    ] {
        let body = source(relative);
        assert!(
            body.contains(state),
            "{relative} no longer carries its `{state}` state: {why}"
        );
    }

    // The route check reads patterns, so it cannot know what it failed to match.
    // Its absence direction must stay a question, never an instruction.
    // Comments quote the old wording to explain why the abstain exists, so this
    // reads the code the user actually sees, not the prose about it.
    let drift = source("src/workflows/backend_learn/drift.rs");
    let printed: String = drift
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !printed.contains("delete the operation"),
        "an absence found by a pattern must never advise deleting a live operation"
    );
    assert!(
        printed.contains("no route matched in source"),
        "the unmatched direction must be phrased as not-found, not as not-existing"
    );

    // Every family reader, including ones added after this was written. A new
    // reader that silently skips what it cannot read reintroduces the exact bug
    // the parse rewrite existed to remove, and it would do so in a file no
    // named-list ratchet covers.
    let learn =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/workflows/backend_learn");
    let mut readers = 0;
    for entry in std::fs::read_dir(&learn).expect("read backend_learn") {
        let path = entry.expect("entry").path();
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_string();
        if !name.ends_with("_ast.rs") {
            continue;
        }
        readers += 1;
        let body = std::fs::read_to_string(&path).expect("read reader");
        assert!(
            body.contains("files_unreadable") || body.contains("files_unparsed"),
            "{name} extracts from a parse but never counts what it could not read: \
             an unreadable file would be indistinguishable from an empty one"
        );
    }
    assert!(
        readers >= 6,
        "expected a reader per family, found {readers}"
    );
}

#[test]
fn a_multi_schema_project_is_never_narrowed_to_one() {
    // Three bugs have had this exact shape: one code path counts every declared
    // schema and another reads only the first, so the reported total is right
    // while the work covers a fraction of it. It keeps recurring because the
    // reporting path and the consuming path read the schema list separately,
    // and fixing one leaves the other silently narrowed.
    for relative in [
        "src/workflows/doctor.rs",
        "src/workflows/backend_target.rs",
        "src/workflows/backend_learn/drift.rs",
    ] {
        let body = source(relative);
        // Comments here QUOTE the old bug to explain why the guard exists, so
        // this reads the code rather than the prose about it.
        let production: String = body
            .split("#[cfg(test)]")
            .next()
            .unwrap_or(&body)
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        let compact: String = production.chars().filter(|c| !c.is_whitespace()).collect();
        for narrowing in [
            "schemas.first()",
            "schema_paths().first()",
            "schema_paths()?.first()",
            "documents.first()",
            "documents.iter().take(1)",
        ] {
            let needle: String = narrowing.chars().filter(|c| !c.is_whitespace()).collect();
            assert!(
                !compact.contains(&needle),
                "{relative} narrows a multi-schema project to one via `{narrowing}`; \
                 every declared schema must be read"
            );
        }
    }

    // The drift check takes the whole set, so a service split across files is
    // compared against the union rather than against whichever file is first.
    let drift = source("src/workflows/backend_learn/drift.rs");
    for signature in [
        "pub fn declared_routes(documents: &[serde_json::Value])",
        "documents: &[serde_json::Value],",
    ] {
        assert!(
            drift.contains(signature),
            "drift.rs must accept every declared schema (`{signature}`)"
        );
    }
}
