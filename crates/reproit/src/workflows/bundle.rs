//! Signed, encrypted, source-neutral offline support bundles.

use crate::domain::execution::ExecutionVerdict;
use crate::interface::cli::context::{exit_with, Ctx, Exit};
use anyhow::{Context, Result};
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use reproit_protocol::{
    compile_capture_failure, ArtifactPolicy, AssessmentStatus, BundleEncryption,
    BundleEncryptionAlgorithm, BundleSignature, BundleSignatureAlgorithm, CapabilityAssessment,
    CaptureAssessmentScope, CaptureBatch, CaptureDefect, CaptureDefectKind, CollectionMethod,
    ConsentClass, EvidenceArtifact, EvidenceArtifactKind, EvidencePolicy, EvidenceSource,
    FailureObservation, ObservationAuthority, ObservationKind, OccurrenceEnvelope,
    ProcessOperation, RedactionState, ReproductionPackage, ReproductionRequirement,
    RequirementKind, RequirementLevel, SubjectIdentity, UnresolvedRequirement,
    UnresolvedRequirementReason, OCCURRENCE_VERSION, PACKAGE_VERSION, SUPPORT_BUNDLE_VERSION,
};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
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
    capsule_id: String,
    capsule_directory: PathBuf,
    repro_id: String,
}

mod cloud;

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

pub(crate) async fn run_occurrence(ctx: &Ctx, reference: &str) -> Result<ExitCode> {
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
    let mut automatic_plan_id = None;
    if package.assessment.status != AssessmentStatus::Eligible || package.plan.is_none() {
        use crate::adapters::execution::AutomaticCompilation;
        match crate::adapters::execution::compile_package_automatically(&root, &package)? {
            AutomaticCompilation::Compiled(compiled) => {
                let persisted = persist_compiled_package(&root, &directory, reference, *compiled)?;
                automatic_plan_id = persisted.package.plan.as_ref().map(|plan| plan.id.clone());
                package = persisted.package;
                ctx.say("  plan:     compiled from one unambiguous trusted provider");
            }
            AutomaticCompilation::Blocked(blockers) => {
                return report_incomplete_occurrence(ctx, &directory, &package, &blockers);
            }
        }
    }

    let run = crate::adapters::execution::execute(&root, &package).await?;
    let latest = directory.join("latest-run.json");
    std::fs::write(&latest, serde_json::to_vec_pretty(&run)?)
        .with_context(|| format!("writing {}", latest.display()))?;
    ctx.emit(&serde_json::json!({
        "command": "occurrence",
        "occurrenceId": package.occurrence.occurrence_id,
        "automaticPlanId": automatic_plan_id,
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

pub(crate) fn compile(
    ctx: &Ctx,
    reference: &str,
    raw_bindings: &[String],
    identity: &str,
) -> Result<String> {
    let (root, occurrence_directory) = find_occurrence(reference).with_context(|| {
        format!("no imported occurrence `{reference}` in this directory or its ancestors")
    })?;
    let package_path = occurrence_directory.join("package.json");
    let package: ReproductionPackage = serde_json::from_slice(&std::fs::read(&package_path)?)?;
    let bindings = parse_bindings(raw_bindings)?;
    let compiled =
        crate::adapters::execution::compile_package(&root, &package, &bindings, identity)?;
    let persisted = persist_compiled_package(&root, &occurrence_directory, reference, compiled)?;
    let plan = persisted
        .package
        .plan
        .as_ref()
        .context("compiled package omitted its plan")?;
    ctx.emit(&serde_json::json!({
        "command": "plan",
        "occurrenceId": reference,
        "packageId": persisted.package.id,
        "planId": plan.id,
        "capsuleId": persisted.capsule_id,
        "reproId": crate::domain::repro::display_repro_id(&persisted.repro_id),
        "providerCatalog": root.join("reproit.yaml"),
        "capsuleDirectory": persisted.capsule_directory,
    }));
    ctx.say(format!("Compiled occurrence {reference}"));
    ctx.say(format!("  plan:     {}", plan.id));
    ctx.say(format!("  capsule:  {}", persisted.capsule_id));
    ctx.say(format!(
        "  guard:    {} (alias @{reference})",
        crate::domain::repro::display_repro_id(&persisted.repro_id)
    ));
    ctx.say(format!("  run:      reproit {reference}"));
    Ok(persisted.repro_id)
}

fn persist_compiled_package(
    root: &Path,
    occurrence_directory: &Path,
    reference: &str,
    mut compiled: ReproductionPackage,
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
    let capsule_directory = capsule.persist(root)?;
    compiled.capsule = Some(serde_json::to_value(&capsule)?);
    compiled.finalize_id().map_err(protocol_error)?;
    compiled.validate().map_err(protocol_error)?;
    write_json_atomically(&package_path, &compiled)?;

    let repro_id = crate::domain::repro::repro_id(0, &[format!("plan:{}", plan.id)]);
    let repro_directory = crate::domain::repro::repro_dir(root, &repro_id);
    std::fs::create_dir_all(&repro_directory)?;
    let meta = crate::domain::repro::Meta {
        id: repro_id.clone(),
        alias: Some(reference.to_string()),
        status: crate::domain::repro::Status::Quarantined,
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
    };
    crate::domain::repro::save_meta(root, &meta)?;
    write_json_atomically(
        &repro_directory.join("replay.json"),
        &serde_json::json!({"seed": 0, "replay": []}),
    )?;
    write_json_atomically(&repro_directory.join("package.json"), &compiled)?;
    write_json_atomically(&repro_directory.join("plan.json"), &plan)?;
    std::fs::write(repro_directory.join("capsule-id"), &capsule.id)?;
    Ok(PersistedCompilation {
        package: compiled,
        capsule_id: capsule.id,
        capsule_directory,
        repro_id,
    })
}

fn parse_bindings(raw_bindings: &[String]) -> Result<BTreeMap<String, String>> {
    let mut bindings = BTreeMap::new();
    for raw in raw_bindings {
        let (requirement, provider) = raw
            .split_once('=')
            .filter(|(requirement, provider)| !requirement.is_empty() && !provider.is_empty())
            .with_context(|| format!("invalid --bind `{raw}`; expected REQ=PROVIDER"))?;
        if bindings
            .insert(requirement.to_string(), provider.to_string())
            .is_some()
        {
            anyhow::bail!("duplicate binding for requirement `{requirement}`");
        }
    }
    Ok(bindings)
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

fn write_bundle(
    path: &Path,
    manifest: &reproit_protocol::SupportBundleManifest,
    ciphertext: &[u8],
) -> Result<()> {
    let header = serde_json::to_vec(manifest)?;
    if header.len() > MAX_HEADER_BYTES {
        anyhow::bail!("support-bundle header exceeds the 1 MiB limit");
    }
    if path.exists() {
        anyhow::bail!("support bundle {} already exists", path.display());
    }
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let filename = path
        .file_name()
        .context("support-bundle output path has no filename")?
        .to_string_lossy();
    let temporary = parent.join(format!(".{filename}.{}.tmp", std::process::id()));
    let write_result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .with_context(|| format!("creating {}", temporary.display()))?;
        file.write_all(MAGIC)?;
        file.write_all(&(header.len() as u32).to_be_bytes())?;
        file.write_all(&header)?;
        file.write_all(ciphertext)?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)
            .with_context(|| format!("installing {}", path.display()))?;
        Ok(())
    })();
    if write_result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    write_result
}

fn read_bundle(path: &Path) -> Result<ParsedBundle> {
    let mut file =
        std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let metadata = file.metadata()?;
    if metadata.len() > (MAX_PLAINTEXT_BYTES * 2) as u64 {
        anyhow::bail!("support bundle exceeds the local import limit");
    }
    let mut magic = vec![0u8; MAGIC.len()];
    file.read_exact(&mut magic)?;
    if magic != MAGIC {
        anyhow::bail!("not a Reproit support bundle");
    }
    let mut length = [0u8; 4];
    file.read_exact(&mut length)?;
    let header_bytes = u32::from_be_bytes(length) as usize;
    if header_bytes == 0 || header_bytes > MAX_HEADER_BYTES {
        anyhow::bail!("invalid support-bundle header length");
    }
    let mut header = vec![0u8; header_bytes];
    file.read_exact(&mut header)?;
    let manifest: reproit_protocol::SupportBundleManifest =
        serde_json::from_slice(&header).context("parsing support-bundle manifest")?;
    manifest.validate().map_err(protocol_error)?;
    let mut ciphertext = Vec::new();
    file.read_to_end(&mut ciphertext)?;
    if ciphertext.is_empty() {
        anyhow::bail!("support bundle has no encrypted payload");
    }
    Ok(ParsedBundle {
        manifest,
        ciphertext,
    })
}

fn verify_bundle(bundle: &ParsedBundle, path: &Path) -> Result<()> {
    let payload_hash = sha256_hex(&bundle.ciphertext);
    if bundle.manifest.payload_sha256 != format!("sha256:{payload_hash}") {
        anyhow::bail!("support-bundle payload hash does not match its manifest");
    }
    let public_key = hex_decode::<32>(
        &bundle.manifest.signature.public_key,
        "signature public key",
    )?;
    let trusted = trusted_signer(path)?;
    if public_key != trusted {
        anyhow::bail!("support-bundle signer does not match the independently trusted key");
    }
    let signature_bytes =
        hex_decode::<64>(&bundle.manifest.signature.signature, "bundle signature")?;
    let verifying_key =
        VerifyingKey::from_bytes(&public_key).context("invalid bundle verifying key")?;
    let signature = Signature::from_bytes(&signature_bytes);
    verifying_key
        .verify(
            &bundle.manifest.signing_bytes().map_err(protocol_error)?,
            &signature,
        )
        .context("support-bundle signature verification failed")
}

fn incomplete_package(occurrence: &OccurrenceEnvelope) -> Result<ReproductionPackage> {
    let requirement = ReproductionRequirement {
        id: "req_current_checkout_process".into(),
        level: RequirementLevel::Required,
        requirement: RequirementKind::Process {
            role: occurrence.subject.component.clone(),
            operation: ProcessOperation::Launch,
        },
        evidence_artifact_ids: vec![],
    };
    let assessment = CapabilityAssessment {
        occurrence_id: occurrence.occurrence_id.clone(),
        status: AssessmentStatus::Incomplete,
        requirements: vec![requirement.clone()],
        unresolved: vec![UnresolvedRequirement {
            requirement_id: requirement.id,
            reason: UnresolvedRequirementReason::MissingEvidence,
            detail: "bind the occurrence to a checkout-owned process provider and exact oracle"
                .into(),
        }],
    };
    let mut package = ReproductionPackage {
        version: PACKAGE_VERSION,
        id: String::new(),
        occurrence: occurrence.clone(),
        assessment,
        plan: None,
        capsule: None,
        legacy: None,
    };
    package.finalize_id().map_err(protocol_error)?;
    package.validate().map_err(protocol_error)?;
    Ok(package)
}

fn encryption_key() -> Result<([u8; 32], bool)> {
    if let Ok(value) = std::env::var(ENCRYPTION_KEY_ENV) {
        return Ok((hex_decode(&value, ENCRYPTION_KEY_ENV)?, false));
    }
    let mut key = [0u8; 32];
    getrandom::fill(&mut key).context("generating support-bundle key")?;
    Ok((key, true))
}

fn signing_key() -> Result<(SigningKey, bool)> {
    if let Ok(value) = std::env::var(SIGNING_KEY_ENV) {
        return Ok((
            SigningKey::from_bytes(&hex_decode(&value, SIGNING_KEY_ENV)?),
            false,
        ));
    }
    let mut key = [0u8; 32];
    getrandom::fill(&mut key).context("generating support-bundle signing key")?;
    Ok((SigningKey::from_bytes(&key), true))
}

fn trusted_signer(bundle_path: &Path) -> Result<[u8; 32]> {
    if let Ok(value) = std::env::var(TRUSTED_SIGNER_ENV) {
        return hex_decode(&value, TRUSTED_SIGNER_ENV);
    }
    if let Ok(value) = std::env::var(SIGNING_KEY_ENV) {
        let signing = SigningKey::from_bytes(&hex_decode(&value, SIGNING_KEY_ENV)?);
        return Ok(*signing.verifying_key().as_bytes());
    }
    let path = signer_path(bundle_path);
    let value = std::fs::read_to_string(&path).with_context(|| {
        format!(
            "reading {}; set {TRUSTED_SIGNER_ENV} when the signer key was transferred separately",
            path.display()
        )
    })?;
    hex_decode(value.trim(), "trusted signer file")
}

fn read_import_key(bundle_path: &Path) -> Result<[u8; 32]> {
    if let Ok(value) = std::env::var(ENCRYPTION_KEY_ENV) {
        return hex_decode(&value, ENCRYPTION_KEY_ENV);
    }
    let path = key_path(bundle_path);
    let value = std::fs::read_to_string(&path).with_context(|| {
        format!(
            "reading {}; set {ENCRYPTION_KEY_ENV} when the key was transferred separately",
            path.display()
        )
    })?;
    hex_decode(value.trim(), "bundle key file")
}

fn write_private_key(path: &Path, key: &[u8; 32]) -> Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .with_context(|| format!("creating {}", path.display()))?;
    writeln!(file, "{}", hex_encode(key))?;
    file.sync_all()?;
    Ok(())
}

fn key_path(bundle_path: &Path) -> PathBuf {
    PathBuf::from(format!("{}.key", bundle_path.display()))
}

fn signer_path(bundle_path: &Path) -> PathBuf {
    PathBuf::from(format!("{}.signer", bundle_path.display()))
}

fn occurrence_id(args: &CollectArgs, artifacts: &[EvidenceArtifact]) -> String {
    let mut digest = Sha256::new();
    digest.update(args.product.as_bytes());
    digest.update([0]);
    digest.update(args.component.as_bytes());
    digest.update([0]);
    digest.update(args.summary.as_bytes());
    for artifact in artifacts {
        digest.update(artifact.id.as_bytes());
    }
    format!("occ_{}", &hex_encode(&digest.finalize())[..20])
}

fn artifact_kind(path: &Path) -> EvidenceArtifactKind {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "dmp" | "mdmp" => EvidenceArtifactKind::CrashDump,
        "core" => EvidenceArtifactKind::CoreDump,
        "json" | "jsonl" => EvidenceArtifactKind::StructuredLog,
        "log" | "txt" => EvidenceArtifactKind::TextLog,
        "trace" | "otlp" => EvidenceArtifactKind::TraceGraph,
        "png" | "jpg" | "jpeg" => EvidenceArtifactKind::Screenshot,
        "mp4" | "mov" | "webm" => EvidenceArtifactKind::Recording,
        _ => EvidenceArtifactKind::Other,
    }
}

fn media_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "json" => "application/json",
        "jsonl" => "application/x-ndjson",
        "log" | "txt" => "text/plain",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "mp4" => "video/mp4",
        _ => "application/octet-stream",
    }
}

fn protocol_error(error: reproit_protocol::ProtocolError) -> anyhow::Error {
    anyhow::anyhow!("{error}")
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex_encode(&Sha256::digest(bytes))
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write;
        write!(&mut output, "{byte:02x}").unwrap();
    }
    output
}

fn hex_decode<const N: usize>(value: &str, field: &str) -> Result<[u8; N]> {
    if value.len() != N * 2 {
        anyhow::bail!("{field} must contain exactly {} hexadecimal bytes", N);
    }
    let decoded = (0..N)
        .map(|index| {
            u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)
                .with_context(|| format!("decoding {field}"))
        })
        .collect::<Result<Vec<_>>>()?;
    decoded
        .try_into()
        .map_err(|_| anyhow::anyhow!("{field} has the wrong length"))
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
}
