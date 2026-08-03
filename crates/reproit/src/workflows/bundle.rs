//! Signed, encrypted, source-neutral offline support bundles.

use crate::domain::execution::ExecutionVerdict;
use crate::interface::cli::context::{exit_with, Ctx, Exit};
use anyhow::{Context, Result};
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use reproit_protocol::{
    backend_capture_from_batch, compile_capture_failure, ArtifactPolicy, AssessmentStatus,
    BundleEncryption, BundleEncryptionAlgorithm, BundleSignature, BundleSignatureAlgorithm,
    CapabilityAssessment, CaptureAssessmentScope, CaptureBatch, CaptureDefect, CaptureDefectKind,
    CollectionMethod, ConsentClass, EvidenceArtifact, EvidenceArtifactKind, EvidencePolicy,
    EvidenceSource, FailureObservation, ObservationAuthority, ObservationKind, OccurrenceEnvelope,
    ProcessOperation, RedactionState, ReproductionPackage, ReproductionRequirement,
    RequirementKind, RequirementLevel, SubjectIdentity, UnresolvedRequirement,
    UnresolvedRequirementReason, OCCURRENCE_VERSION, PACKAGE_VERSION, SUPPORT_BUNDLE_VERSION,
};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs::OpenOptions;
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const MAGIC: &[u8] = b"REPROIT-RPB\x01";
const MAX_HEADER_BYTES: usize = 1024 * 1024;
const MAX_ARTIFACTS: usize = 128;
const MAX_PLAINTEXT_BYTES: usize = 64 * 1024 * 1024;
const ENCRYPTION_KEY_ENV: &str = "REPROIT_BUNDLE_ENCRYPTION_KEY";
const SIGNING_KEY_ENV: &str = "REPROIT_BUNDLE_SIGNING_KEY";
const TRUSTED_SIGNER_ENV: &str = "REPROIT_BUNDLE_TRUSTED_SIGNER";

pub(crate) struct CollectArgs {
    pub(crate) output: PathBuf,
    pub(crate) product: String,
    pub(crate) component: String,
    pub(crate) platform: Option<String>,
    pub(crate) summary: String,
    pub(crate) artifacts: Vec<PathBuf>,
    pub(crate) exportable: bool,
    pub(crate) retention_class: String,
}

#[derive(Debug)]
struct ParsedBundle {
    manifest: reproit_protocol::SupportBundleManifest,
    ciphertext: Vec<u8>,
}

struct CollectedArtifacts {
    records: Vec<EvidenceArtifact>,
    contents: Vec<(String, Vec<u8>)>,
}

struct PersistedCompilation {
    package: ReproductionPackage,
    repro_id: String,
}

mod cloud;
mod format;
use format::*;

pub(crate) fn collect(ctx: &Ctx, args: CollectArgs) -> Result<()> {
    if args.artifacts.len() > MAX_ARTIFACTS {
        anyhow::bail!("support bundles accept at most {MAX_ARTIFACTS} artifacts");
    }
    let collected = collect_artifacts(&args)?;
    let now = chrono::Utc::now().to_rfc3339();
    let occurrence_id = occurrence_id(&args, &collected.records);
    let occurrence = OccurrenceEnvelope {
        version: OCCURRENCE_VERSION,
        occurrence_id,
        source: EvidenceSource::SupportBundle,
        subject: SubjectIdentity {
            product: args.product,
            component: args.component,
            platform: args.platform,
        },
        observed_at: now.clone(),
        received_at: now,
        deployment: None,
        observations: vec![FailureObservation {
            kind: ObservationKind::UserReport,
            authority: ObservationAuthority::SourceClaim,
            summary: args.summary,
            signature: None,
            observation_point: None,
            artifact_ids: collected
                .records
                .iter()
                .map(|artifact| artifact.id.clone())
                .collect(),
        }],
        artifacts: collected.records,
        capture_defects: if collected.contents.is_empty() {
            vec![CaptureDefect {
                kind: CaptureDefectKind::Unavailable,
                detail: "the support bundle contains no evidence artifacts".into(),
                artifact_id: None,
            }]
        } else {
            vec![]
        },
        policy: EvidencePolicy {
            consent: if args.exportable {
                ConsentClass::SupportExport
            } else {
                ConsentClass::LocalAnalysis
            },
            retention_class: args.retention_class,
        },
    };
    occurrence
        .validate()
        .map_err(|error| anyhow::anyhow!("invalid support occurrence: {error}"))?;

    let plaintext = archive_artifacts(&collected.contents)?;
    if plaintext.len() > MAX_PLAINTEXT_BYTES {
        anyhow::bail!(
            "support bundle payload exceeds the {} MiB local collection limit",
            MAX_PLAINTEXT_BYTES / (1024 * 1024)
        );
    }
    let (encryption_key, generated_key) = encryption_key()?;
    let mut nonce = [0u8; 24];
    getrandom::fill(&mut nonce).context("generating support-bundle nonce")?;
    let cipher = XChaCha20Poly1305::new((&encryption_key).into());
    let ciphertext = cipher
        .encrypt(XNonce::from_slice(&nonce), plaintext.as_ref())
        .map_err(|_| anyhow::anyhow!("encrypting support bundle"))?;
    let payload_hash = sha256_hex(&ciphertext);
    let (signing_key, generated_signer) = signing_key()?;
    let verifying_key = signing_key.verifying_key();
    let mut manifest = reproit_protocol::SupportBundleManifest {
        version: SUPPORT_BUNDLE_VERSION,
        bundle_id: format!("rpb_{payload_hash}"),
        occurrence,
        encryption: BundleEncryption {
            algorithm: BundleEncryptionAlgorithm::Xchacha20Poly1305,
            recipient_key_id: format!("key_{}", &sha256_hex(&encryption_key)[..16]),
            nonce: hex_encode(&nonce),
        },
        payload_sha256: format!("sha256:{payload_hash}"),
        signature: BundleSignature {
            algorithm: BundleSignatureAlgorithm::Ed25519,
            key_id: format!("sig_{}", &sha256_hex(verifying_key.as_bytes())[..16]),
            public_key: hex_encode(verifying_key.as_bytes()),
            signature: "0".repeat(128),
        },
    };
    let signature = signing_key.sign(&manifest.signing_bytes().map_err(protocol_error)?);
    manifest.signature.signature = hex_encode(&signature.to_bytes());
    manifest.validate().map_err(protocol_error)?;
    let key_path = if generated_key {
        let path = key_path(&args.output);
        write_private_key(&path, &encryption_key)?;
        Some(path)
    } else {
        None
    };
    let signer_path = if generated_signer {
        let path = signer_path(&args.output);
        if let Err(error) = write_private_key(&path, verifying_key.as_bytes()) {
            if let Some(path) = &key_path {
                let _ = std::fs::remove_file(path);
            }
            return Err(error);
        }
        Some(path)
    } else {
        None
    };
    if let Err(error) = write_bundle(&args.output, &manifest, &ciphertext) {
        if let Some(path) = &key_path {
            let _ = std::fs::remove_file(path);
        }
        if let Some(path) = &signer_path {
            let _ = std::fs::remove_file(path);
        }
        return Err(error);
    }
    ctx.emit(&serde_json::json!({
        "command": "collect",
        "bundle": args.output,
        "bundleId": manifest.bundle_id,
        "occurrenceId": manifest.occurrence.occurrence_id,
        "artifacts": manifest.occurrence.artifacts.len(),
        "keyFile": key_path,
        "trustedSignerFile": signer_path,
    }));
    ctx.say(format!("Collected {}", args.output.display()));
    ctx.say(format!(
        "  occurrence: {}",
        manifest.occurrence.occurrence_id
    ));
    ctx.say(format!(
        "  artifacts:  {}",
        manifest.occurrence.artifacts.len()
    ));
    if let Some(path) = key_path {
        ctx.say(format!(
            "  key:        {} (transfer separately from the bundle)",
            path.display()
        ));
    }
    if let Some(path) = signer_path {
        ctx.say(format!(
            "  signer:     {} (transfer separately from the bundle)",
            path.display()
        ));
    }
    Ok(())
}

pub(crate) fn inspect(ctx: &Ctx, path: &Path) -> Result<()> {
    let bundle = read_bundle(path)?;
    verify_bundle(&bundle, path)?;
    ctx.emit(&serde_json::to_value(&bundle.manifest)?);
    ctx.say(format!("Bundle {}", bundle.manifest.bundle_id));
    ctx.say(format!(
        "  occurrence: {}",
        bundle.manifest.occurrence.occurrence_id
    ));
    ctx.say(format!(
        "  subject:    {}/{}",
        bundle.manifest.occurrence.subject.product, bundle.manifest.occurrence.subject.component
    ));
    ctx.say(format!(
        "  artifacts:  {}",
        bundle.manifest.occurrence.artifacts.len()
    ));
    ctx.say("  signature:  valid");
    Ok(())
}

pub(crate) fn import(ctx: &Ctx, path: &Path) -> Result<String> {
    let bundle = read_bundle(path)?;
    verify_bundle(&bundle, path)?;
    let encryption_key = read_import_key(path)?;
    let nonce = hex_decode::<24>(&bundle.manifest.encryption.nonce, "bundle nonce")?;
    let cipher = XChaCha20Poly1305::new((&encryption_key).into());
    let plaintext = cipher
        .decrypt(XNonce::from_slice(&nonce), bundle.ciphertext.as_ref())
        .map_err(|_| anyhow::anyhow!("bundle decryption failed: wrong key or changed payload"))?;
    if plaintext.len() > MAX_PLAINTEXT_BYTES {
        anyhow::bail!("decrypted support bundle exceeds the local import limit");
    }
    let root = std::env::current_dir()?;
    let occurrence_id = &bundle.manifest.occurrence.occurrence_id;
    let parent = root.join(".reproit/occurrences");
    std::fs::create_dir_all(&parent)?;
    let directory = parent.join(occurrence_id);
    if directory.exists() {
        anyhow::bail!("occurrence {occurrence_id} is already imported");
    }
    let staging = parent.join(format!(".{occurrence_id}.{}.staging", std::process::id()));
    std::fs::create_dir(&staging)?;
    let package = incomplete_package(&bundle.manifest.occurrence)?;
    let import_result = (|| -> Result<()> {
        let artifact_directory = staging.join("artifacts");
        let imported = unpack_artifacts(&plaintext, &artifact_directory)?;
        verify_imported_artifacts(&artifact_directory, &bundle.manifest.occurrence, &imported)?;
        write_json_atomically(&staging.join("manifest.json"), &bundle.manifest)?;
        write_json_atomically(&staging.join("package.json"), &package)?;
        std::fs::rename(&staging, &directory)?;
        Ok(())
    })();
    if import_result.is_err() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    import_result?;
    ctx.emit(&serde_json::json!({
        "command": "import",
        "bundle": path,
        "bundleId": bundle.manifest.bundle_id,
        "occurrenceId": occurrence_id,
        "status": package.assessment.status,
        "missing": package.assessment.unresolved,
        "directory": directory,
    }));
    ctx.say(format!("Imported occurrence {occurrence_id}"));
    ctx.say(format!("  evidence: {}", directory.display()));
    ctx.say(format!("  reproduce: reproit {occurrence_id}"));
    ctx.say("  planning:  automatic when exactly one trusted provider matches");
    Ok(occurrence_id.clone())
}

pub(crate) async fn run_occurrence(
    ctx: &Ctx,
    config_path: Option<&Path>,
    reference: &str,
    no_run: bool,
    exec_override: Option<&str>,
    auto: bool,
) -> Result<ExitCode> {
    if find_occurrence(reference).is_none() {
        cloud::pull_cloud_occurrence(ctx, reference).await?;
    }
    let (root, directory) = find_occurrence(reference)
        .with_context(|| format!("no local or Cloud occurrence `{reference}` is available"))?;
    let package_path = directory.join("package.json");
    let mut package: ReproductionPackage = serde_json::from_slice(
        &std::fs::read(&package_path)
            .with_context(|| format!("reading {}", package_path.display()))?,
    )
    .with_context(|| format!("parsing {}", package_path.display()))?;
    package.validate().map_err(protocol_error)?;
    if no_run {
        ctx.emit(&serde_json::json!({
            "command": "occurrence pull",
            "occurrenceId": package.occurrence.occurrence_id,
            "status": package.assessment.status,
            "packageId": package.id,
            "directory": directory,
        }));
        ctx.say(format!("Pulled occurrence {reference}"));
        ctx.say(format!("  evidence: {}", directory.display()));
        ctx.say(format!("  run:      reproit {reference}"));
        return Ok(ExitCode::SUCCESS);
    }
    if package.plan.is_none()
        && package
            .legacy
            .as_ref()
            .is_some_and(|legacy| !legacy.actions.is_empty())
    {
        persist_legacy_occurrence(&root, &package, reference)?;
        ctx.say("  replay:   using the captured action path through the trusted app adapter");
        return super::check::run(
            ctx,
            config_path,
            super::check::CheckArgs {
                repro: Some(reference.to_string()),
                devices: 1,
                kind: None,
                runs: Some(1),
                junit: None,
                service: Vec::new(),
                strict: false,
                locale: None,
                target: None,
                device: None,
                record_video: false,
                flicker: false,
                changed: None,
                update_baseline: false,
                exec: None,
                auto: true,
            },
        )
        .await;
    }
    // A backend occurrence carries its own replayable evidence: re-evaluate
    // the projected capture offline under `check`'s verdict contract instead
    // of compiling an execution plan.
    let backend_capture = directory.join("backend-capture.json");
    if backend_capture.is_file() {
        // Hermetic re-execution when the capture recorded dependency
        // exchanges AND a boot command exists: `--exec` wins, else
        // `backend.exec` from reproit.yaml. Both are user-authored; a
        // capture can never supply a command.
        if super::backend_headless::capture_has_exchanges(&backend_capture) {
            let exec = match exec_override {
                Some(exec) => Some(exec.to_string()),
                None => super::backend_target::find(config_path)?
                    .and_then(|project| project.config.exec.clone()),
            };
            if let Some(exec) = exec {
                ctx.say("  replay:   hermetic re-execution with the recorded exchanges");
                return super::backend_headless::check_capture_exec(
                    ctx,
                    &backend_capture,
                    &exec,
                    auto,
                )
                .await;
            }
            ctx.say(
                "  replay:   this capture carries recorded exchanges; set backend.exec in \
                 reproit.yaml (or pass --exec) to re-execute it hermetically. Falling back \
                 to offline log re-evaluation",
            );
        } else if let Some(exec) = exec_override {
            // An explicit --exec on an exchange-less capture must fail closed
            // with the named reason, never silently downgrade to a mode the
            // caller did not ask for.
            return super::backend_headless::check_capture_exec(ctx, &backend_capture, exec, auto)
                .await;
        } else {
            ctx.say(
                "  replay:   offline log re-evaluation of the recorded events (NOT hermetic \
                 re-execution: this capture carries no recorded dependency exchanges)",
            );
        }
        return super::backend_headless::check_capture(ctx, &backend_capture);
    }
    let mut automatic_plan_id = None;
    if package.assessment.status != AssessmentStatus::Eligible || package.plan.is_none() {
        use crate::adapters::execution::AutomaticCompilation;
        match crate::adapters::execution::compile_package_automatically(&root, &package)? {
            AutomaticCompilation::Compiled(compiled) => {
                let persisted = persist_compiled_package(
                    &root,
                    &directory,
                    reference,
                    *compiled,
                    crate::domain::repro::Status::Quarantined,
                )?;
                automatic_plan_id = persisted.package.plan.as_ref().map(|plan| plan.id.clone());
                package = persisted.package;
                ctx.say("  plan:     compiled from one unambiguous trusted provider");
            }
            AutomaticCompilation::Blocked(blockers) => {
                return report_incomplete_occurrence(ctx, &directory, &package, &blockers);
            }
        }
    }

    // The honest distinction, stated where the verdict is: a compiled plan
    // re-executes against a LIVE target (dependencies really run), which is
    // not the hermetic re-execution an exchange-carrying capture gets.
    ctx.say("  replay:   live-target re-send through the compiled plan (not hermetic)");
    let run = crate::adapters::execution::execute(&root, &package).await?;
    let latest = directory.join("latest-run.json");
    std::fs::write(&latest, serde_json::to_vec_pretty(&run)?)
        .with_context(|| format!("writing {}", latest.display()))?;
    ctx.emit(&serde_json::json!({
        "command": "occurrence",
        "occurrenceId": package.occurrence.occurrence_id,
        "automaticPlanId": automatic_plan_id,
        "mode": "live-resend",
        "verdict": run.verdict,
        "run": run,
    }));
    ctx.say(format!("Occurrence {}", package.occurrence.occurrence_id));
    ctx.say(format!("  verdict:  {:?}", run.verdict));
    let exit = match run.verdict {
        ExecutionVerdict::Reproduced => Exit::Regression,
        ExecutionVerdict::NotReproduced => Exit::Clean,
        ExecutionVerdict::Flaky => Exit::Flaky,
        ExecutionVerdict::Stale
        | ExecutionVerdict::Incomplete
        | ExecutionVerdict::Unsupported
        | ExecutionVerdict::DifferentFailure
        | ExecutionVerdict::InfrastructureFailed => Exit::Stale,
    };
    Ok(exit_with(exit))
}

fn persist_legacy_occurrence(
    root: &Path,
    package: &ReproductionPackage,
    alias: &str,
) -> Result<String> {
    let legacy = package
        .legacy
        .as_ref()
        .context("legacy occurrence omitted its replay")?;
    let observation = package
        .occurrence
        .observations
        .first()
        .context("legacy occurrence omitted its observation")?;
    let seed = 0;
    let id = crate::domain::repro::repro_id(seed, &legacy.actions);
    let meta = crate::domain::repro::Meta {
        id: id.clone(),
        alias: Some(alias.to_string()),
        status: crate::domain::repro::Status::Quarantined,
        seed,
        created: chrono::Utc::now().to_rfc3339(),
        last_checked: None,
        last_result: None,
        trigger_index: Some(legacy.actions.len()),
        trigger_sig: observation.signature.clone(),
        trigger_selector: None,
        trigger_fingerprint: None,
        oracle: Some(observation_oracle(observation.kind).to_string()),
        record_url: None,
        record_action: None,
    };
    let fixture = crate::domain::fixture::synthesize(&legacy.fixture);
    let replay = super::triage::build_replay_json(seed, &legacy.actions, &fixture);
    let directory = crate::domain::repro::repro_dir(root, &id);
    std::fs::create_dir_all(&directory)?;
    write_json_atomically(&directory.join("replay.json"), &replay)?;
    crate::domain::repro::save_meta(root, &meta)?;
    write_json_atomically(&directory.join("package.json"), package)?;
    Ok(id)
}

fn observation_oracle(kind: ObservationKind) -> &'static str {
    match kind {
        ObservationKind::Exception | ObservationKind::Crash | ObservationKind::Exit => "crash",
        ObservationKind::Hang => "hang",
        ObservationKind::ContractViolation => "contract",
        ObservationKind::DataCorruption => "backend-data-loss",
        ObservationKind::Performance => "jank",
        ObservationKind::UserReport => "tester-capture",
        ObservationKind::Diagnostic => "diagnostic",
    }
}

fn report_incomplete_occurrence(
    ctx: &Ctx,
    directory: &Path,
    package: &ReproductionPackage,
    planning_blockers: &[crate::adapters::execution::CompilationBlocker],
) -> Result<ExitCode> {
    let missing = &package.assessment.unresolved;
    ctx.emit(&serde_json::json!({
        "command": "occurrence",
        "occurrenceId": package.occurrence.occurrence_id,
        "status": package.assessment.status,
        "missing": missing,
        "planningBlockers": planning_blockers,
        "evidence": directory,
    }));
    ctx.say(format!("Occurrence {}", package.occurrence.occurrence_id));
    ctx.say(format!("  status:   {:?}", package.assessment.status));
    for blocker in planning_blockers {
        ctx.say(format!("  blocked:  {blocker}"));
    }
    if planning_blockers.is_empty() {
        for unresolved in missing {
            ctx.say(format!(
                "  missing:  {}: {}",
                unresolved.requirement_id, unresolved.detail
            ));
        }
    }
    Ok(exit_with(Exit::Stale))
}

pub(crate) async fn keep_occurrence(
    ctx: &Ctx,
    reference: &str,
    alias: Option<&str>,
    strict: bool,
) -> Result<ExitCode> {
    if find_occurrence(reference).is_none() {
        cloud::pull_cloud_occurrence(ctx, reference).await?;
    }
    let (root, occurrence_directory) = find_occurrence(reference)
        .with_context(|| format!("no local or Cloud occurrence `{reference}` is available"))?;
    let package_path = occurrence_directory.join("package.json");
    let package: ReproductionPackage = serde_json::from_slice(
        &std::fs::read(&package_path)
            .with_context(|| format!("reading {}", package_path.display()))?,
    )
    .with_context(|| format!("parsing {}", package_path.display()))?;
    package.validate().map_err(protocol_error)?;
    let compiled =
        if package.assessment.status == AssessmentStatus::Eligible && package.plan.is_some() {
            package
        } else {
            use crate::adapters::execution::AutomaticCompilation;
            match crate::adapters::execution::compile_package_automatically(&root, &package)? {
                AutomaticCompilation::Compiled(compiled) => *compiled,
                AutomaticCompilation::Blocked(blockers) => {
                    let detail = blockers
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join("; ");
                    anyhow::bail!(
                        "occurrence `{reference}` cannot be kept until it has one exact trusted \
                     reproduction plan: {detail}"
                    );
                }
            }
        };
    let requested_status = if strict {
        crate::domain::repro::Status::Required
    } else {
        crate::domain::repro::Status::Quarantined
    };
    let persisted = persist_compiled_package(
        &root,
        &occurrence_directory,
        alias.unwrap_or(reference),
        compiled,
        requested_status,
    )?;
    let meta = crate::domain::repro::load_meta(&root, &persisted.repro_id)
        .context("kept occurrence omitted its guard metadata")?;
    let display_id = crate::domain::repro::display_repro_id(&persisted.repro_id);
    let directory = crate::domain::repro::repro_dir(&root, &persisted.repro_id);
    ctx.emit(&serde_json::json!({
        "command": "keep",
        "source": "occurrence",
        "occurrenceId": reference,
        "id": display_id,
        "alias": meta.alias,
        "status": meta.status.as_str(),
        "directory": directory,
        "planId": persisted.package.plan.as_ref().map(|plan| &plan.id),
    }));
    ctx.say(format!("Kept occurrence {reference} as {display_id}"));
    ctx.say(format!("  status: {}", meta.status.as_str()));
    ctx.say(format!("  guard:  {}", directory.display()));
    Ok(ExitCode::SUCCESS)
}

fn persist_compiled_package(
    root: &Path,
    occurrence_directory: &Path,
    alias: &str,
    mut compiled: ReproductionPackage,
    requested_status: crate::domain::repro::Status,
) -> Result<PersistedCompilation> {
    let package_path = occurrence_directory.join("package.json");
    let plan = compiled
        .plan
        .as_ref()
        .context("compiled package omitted its plan")?
        .clone();
    let identity = &plan.observation.identity;
    let observation = compiled
        .occurrence
        .observations
        .first()
        .context("compiled occurrence has no observation")?;
    let mut capsule = crate::domain::capsule::Capsule::new(
        compiled.occurrence.subject.product.clone(),
        crate::domain::capsule::FindingIdentity {
            oracle: format!("{:?}", observation.kind).to_ascii_lowercase(),
            invariant: "exact-occurrence".into(),
            kind: format!("{:?}", observation.kind).to_ascii_lowercase(),
            message: observation.summary.clone(),
            frame: observation.observation_point.clone().unwrap_or_default(),
            trigger: plan.target.clone(),
            boundary: Some(identity.clone()),
        },
    );
    capsule.occurrence = Some(compiled.occurrence.clone());
    capsule.assessment = Some(compiled.assessment.clone());
    capsule.reproduction_plan = Some(plan.clone());
    capsule.persist(root)?;
    compiled.capsule = Some(serde_json::to_value(&capsule)?);
    compiled.finalize_id().map_err(protocol_error)?;
    compiled.validate().map_err(protocol_error)?;
    write_json_atomically(&package_path, &compiled)?;

    let repro_id = crate::domain::repro::guard_repro_id(&compiled.occurrence.occurrence_id);
    let repro_directory = crate::domain::repro::repro_dir(root, &repro_id);
    std::fs::create_dir_all(&repro_directory)?;
    let meta = if let Some(mut existing) = crate::domain::repro::load_meta(root, &repro_id) {
        existing.alias = Some(alias.to_string());
        if requested_status == crate::domain::repro::Status::Required {
            existing.status = requested_status;
        }
        existing
    } else {
        crate::domain::repro::Meta {
            id: repro_id.clone(),
            alias: Some(alias.to_string()),
            status: requested_status,
            seed: 0,
            created: chrono::Utc::now().to_rfc3339(),
            last_checked: None,
            last_result: None,
            trigger_index: Some(0),
            trigger_sig: Some(identity.clone()),
            trigger_selector: None,
            trigger_fingerprint: None,
            oracle: Some("exact-occurrence".into()),
            record_url: None,
            record_action: None,
        }
    };
    crate::domain::repro::save_meta(root, &meta)?;
    write_json_atomically(
        &repro_directory.join("replay.json"),
        &serde_json::json!({"seed": 0, "replay": []}),
    )?;
    write_json_atomically(&repro_directory.join("package.json"), &compiled)?;
    write_json_atomically(&repro_directory.join("plan.json"), &plan)?;
    crate::adapters::execution::persist_plan_catalog(root, &compiled, &repro_directory)?;
    std::fs::write(repro_directory.join("capsule-id"), &capsule.id)?;
    Ok(PersistedCompilation {
        package: compiled,
        repro_id,
    })
}

fn write_json_atomically(path: &Path, value: &impl serde::Serialize) -> Result<()> {
    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, serde_json::to_vec_pretty(value)?)
        .with_context(|| format!("writing {}", temporary.display()))?;
    std::fs::rename(&temporary, path).with_context(|| format!("replacing {}", path.display()))
}

fn write_private_bytes_atomically(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("artifact path has no parent")?;
    let temporary = parent.join(format!(
        ".{}.tmp",
        path.file_name().unwrap().to_string_lossy()
    ));
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&temporary)
        .with_context(|| format!("creating {}", temporary.display()))?;
    file.write_all(bytes)
        .with_context(|| format!("writing {}", temporary.display()))?;
    file.sync_all()
        .with_context(|| format!("syncing {}", temporary.display()))?;
    std::fs::rename(&temporary, path).with_context(|| format!("installing {}", path.display()))
}

fn find_occurrence(reference: &str) -> Option<(PathBuf, PathBuf)> {
    let mut root = std::env::current_dir().ok()?;
    loop {
        let directory = root.join(".reproit/occurrences").join(reference);
        if directory.join("package.json").is_file() {
            return Some((root, directory));
        }
        if !root.pop() {
            return None;
        }
    }
}

fn collect_artifacts(args: &CollectArgs) -> Result<CollectedArtifacts> {
    let mut records = Vec::with_capacity(args.artifacts.len());
    let mut contents = Vec::with_capacity(args.artifacts.len());
    let mut total_bytes = 0usize;
    for path in &args.artifacts {
        let metadata =
            std::fs::metadata(path).with_context(|| format!("reading {}", path.display()))?;
        if !metadata.is_file() {
            anyhow::bail!("support artifact is not a regular file: {}", path.display());
        }
        let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        total_bytes = total_bytes
            .checked_add(bytes.len())
            .context("support artifact size overflow")?;
        if total_bytes > MAX_PLAINTEXT_BYTES {
            anyhow::bail!("support artifacts exceed the local collection limit");
        }
        let hash = sha256_hex(&bytes);
        let id = format!("sha256:{hash}");
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .map(String::from);
        records.push(EvidenceArtifact {
            id: id.clone(),
            kind: artifact_kind(path),
            media_type: media_type(path).into(),
            bytes: bytes.len() as u64,
            policy: if args.exportable {
                ArtifactPolicy::Exportable
            } else {
                ArtifactPolicy::LocalAnalysisOnly
            },
            redaction: if args.exportable {
                RedactionState::RedactedAtSource
            } else {
                RedactionState::UnredactedRestricted
            },
            collection: CollectionMethod::SupportCollector,
            encryption_key_id: None,
            name,
        });
        contents.push((id, bytes));
    }
    Ok(CollectedArtifacts { records, contents })
}

fn archive_artifacts(artifacts: &[(String, Vec<u8>)]) -> Result<Vec<u8>> {
    let mut builder = tar::Builder::new(Vec::new());
    for (id, bytes) in artifacts {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o600);
        header.set_cksum();
        builder.append_data(
            &mut header,
            format!("artifacts/{}", &id[7..]),
            bytes.as_slice(),
        )?;
    }
    builder.finish()?;
    builder.into_inner().context("finalizing artifact archive")
}

fn unpack_artifacts(plaintext: &[u8], destination: &Path) -> Result<BTreeSet<String>> {
    std::fs::create_dir_all(destination)?;
    let mut archive = tar::Archive::new(Cursor::new(plaintext));
    let mut imported = BTreeSet::new();
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        let components = path.components().collect::<Vec<_>>();
        if components.len() != 2
            || components[0].as_os_str() != "artifacts"
            || components[1].as_os_str().to_str().is_none_or(|name| {
                name.len() != 64 || !name.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
        {
            anyhow::bail!("bundle contains an unsafe artifact path");
        }
        let name = components[1].as_os_str().to_string_lossy().into_owned();
        if !imported.insert(name.clone()) {
            anyhow::bail!("bundle contains a duplicate artifact");
        }
        let output = destination.join(name);
        entry.unpack(&output)?;
    }
    Ok(imported)
}

fn verify_imported_artifacts(
    directory: &Path,
    occurrence: &OccurrenceEnvelope,
    imported: &BTreeSet<String>,
) -> Result<()> {
    let expected = occurrence
        .artifacts
        .iter()
        .map(|artifact| artifact.id[7..].to_string())
        .collect::<BTreeSet<_>>();
    if &expected != imported {
        anyhow::bail!("bundle artifact inventory does not match its encrypted archive");
    }
    for artifact in &occurrence.artifacts {
        let path = directory.join(&artifact.id[7..]);
        let bytes = std::fs::read(&path)
            .with_context(|| format!("verifying imported artifact {}", path.display()))?;
        if bytes.len() as u64 != artifact.bytes || sha256_hex(&bytes) != artifact.id[7..] {
            anyhow::bail!(
                "imported artifact {} failed content verification",
                artifact.id
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artifact_kind_is_source_neutral() {
        assert_eq!(
            artifact_kind(Path::new("service.dmp")),
            EvidenceArtifactKind::CrashDump
        );
        assert_eq!(
            artifact_kind(Path::new("worker.log")),
            EvidenceArtifactKind::TextLog
        );
    }

    #[test]
    fn hex_round_trip_is_exact() {
        let bytes = [7u8; 32];
        assert_eq!(
            hex_decode::<32>(&hex_encode(&bytes), "test").unwrap(),
            bytes
        );
    }

    #[test]
    fn legacy_cloud_occurrence_becomes_a_local_guard_without_a_process_plan() {
        let occurrence_id = "occ_0123456789abcdef";
        let occurrence = OccurrenceEnvelope {
            version: OCCURRENCE_VERSION,
            occurrence_id: occurrence_id.into(),
            source: EvidenceSource::Sentry,
            subject: SubjectIdentity {
                product: "shop".into(),
                component: "checkout".into(),
                platform: Some("web".into()),
            },
            observed_at: "2026-07-01T00:00:00Z".into(),
            received_at: "2026-07-01T00:00:01Z".into(),
            deployment: None,
            observations: vec![FailureObservation {
                kind: ObservationKind::Exception,
                authority: ObservationAuthority::RuntimeDiagnosis,
                summary: "checkout failed".into(),
                signature: Some("TypeError:checkout".into()),
                observation_point: None,
                artifact_ids: vec![],
            }],
            artifacts: vec![],
            capture_defects: vec![],
            policy: EvidencePolicy {
                consent: ConsentClass::LocalAnalysis,
                retention_class: "production".into(),
            },
        };
        let assessment = CapabilityAssessment {
            occurrence_id: occurrence_id.into(),
            status: AssessmentStatus::Eligible,
            requirements: vec![],
            unresolved: vec![],
        };
        let mut package = ReproductionPackage {
            version: PACKAGE_VERSION,
            id: String::new(),
            occurrence,
            assessment,
            plan: None,
            capsule: None,
            legacy: Some(reproit_protocol::LegacyReplay {
                actions: vec!["tap:key:id:checkout".into()],
                fixture: serde_json::json!({"locale": "en-US"}),
                crash_signature: Some("TypeError:checkout".into()),
                start_signature: Some("home".into()),
            }),
        };
        package.finalize_id().unwrap();
        package.validate().unwrap();

        let root = std::env::temp_dir().join(format!(
            "reproit-legacy-occurrence-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let id = persist_legacy_occurrence(&root, &package, occurrence_id).unwrap();
        let directory = crate::domain::repro::repro_dir(&root, &id);
        let meta = crate::domain::repro::load_meta(&root, &id).unwrap();

        assert_eq!(meta.alias.as_deref(), Some(occurrence_id));
        assert_eq!(meta.oracle.as_deref(), Some("crash"));
        assert_eq!(meta.trigger_index, Some(1));
        assert!(directory.join("replay.json").is_file());
        assert!(directory.join("package.json").is_file());

        std::fs::remove_dir_all(root).unwrap();
    }
}
