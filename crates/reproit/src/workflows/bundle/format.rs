//! Support-bundle file format: framing, signing and encryption keys, artifact
//! classification, and the encoding primitives the envelope depends on.
//!
//! Split out of `bundle.rs` so both files stay inside the workspace
//! reviewability bound. Items are `pub(super)` unless the wider crate already
//! depended on them.

use super::*;

pub(super) fn write_bundle(
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

pub(super) fn read_bundle(path: &Path) -> Result<ParsedBundle> {
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

pub(super) fn verify_bundle(bundle: &ParsedBundle, path: &Path) -> Result<()> {
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

pub(super) fn incomplete_package(occurrence: &OccurrenceEnvelope) -> Result<ReproductionPackage> {
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

pub(super) fn encryption_key() -> Result<([u8; 32], bool)> {
    if let Ok(value) = std::env::var(ENCRYPTION_KEY_ENV) {
        return Ok((hex_decode(&value, ENCRYPTION_KEY_ENV)?, false));
    }
    let mut key = [0u8; 32];
    getrandom::fill(&mut key).context("generating support-bundle key")?;
    Ok((key, true))
}

pub(super) fn signing_key() -> Result<(SigningKey, bool)> {
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

pub(super) fn trusted_signer(bundle_path: &Path) -> Result<[u8; 32]> {
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

pub(super) fn read_import_key(bundle_path: &Path) -> Result<[u8; 32]> {
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

pub(super) fn write_private_key(path: &Path, key: &[u8; 32]) -> Result<()> {
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

pub(super) fn key_path(bundle_path: &Path) -> PathBuf {
    PathBuf::from(format!("{}.key", bundle_path.display()))
}

pub(super) fn signer_path(bundle_path: &Path) -> PathBuf {
    PathBuf::from(format!("{}.signer", bundle_path.display()))
}

pub(super) fn occurrence_id(args: &CollectArgs, artifacts: &[EvidenceArtifact]) -> String {
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

pub(super) fn artifact_kind(path: &Path) -> EvidenceArtifactKind {
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

pub(super) fn media_type(path: &Path) -> &'static str {
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

pub(super) fn protocol_error(error: reproit_protocol::ProtocolError) -> anyhow::Error {
    anyhow::anyhow!("{error}")
}

pub(super) fn sha256_hex(bytes: &[u8]) -> String {
    hex_encode(&Sha256::digest(bytes))
}

pub(super) fn hex_encode(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write;
        write!(&mut output, "{byte:02x}").unwrap();
    }
    output
}

pub(super) fn hex_decode<const N: usize>(value: &str, field: &str) -> Result<[u8; N]> {
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
