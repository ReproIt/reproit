use super::*;

const MAX_CLOUD_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
const MAX_CLOUD_ARTIFACT_BYTES: usize = 25 * 1024 * 1024;

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CloudOccurrence {
    #[serde(default)]
    app_id: Option<String>,
    #[serde(default)]
    bucket_id: Option<String>,
    occurrence: OccurrenceEnvelope,
    assessment: CapabilityAssessment,
    #[serde(default)]
    package: Option<ReproductionPackage>,
    #[serde(default)]
    capture: Option<CaptureBatch>,
    #[serde(default)]
    readiness: Option<reproit_protocol::ReadinessAssessment>,
}

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct CloudProvenance {
    pub(super) app_id: String,
    pub(super) bucket_id: String,
    pub(super) cloud_base: String,
}

struct DownloadedArtifact {
    filename: String,
    bytes: Vec<u8>,
}

struct LocalizedOccurrence {
    package: ReproductionPackage,
    capture: Option<CaptureBatch>,
}

pub(super) async fn pull_cloud_occurrence(ctx: &Ctx, reference: &str) -> Result<()> {
    validate_cloud_occurrence_id(reference)?;
    let (cloud, key) = crate::workflows::cloud::cloud_creds(None, None);
    let key = key.with_context(|| {
        format!(
            "occurrence `{reference}` is not local and no Cloud login is configured; \
             run `reproit login`"
        )
    })?;
    let cloud = cloud.unwrap_or_else(|| "https://cloud.reproit.com".into());
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .context("creating Cloud occurrence client")?;
    let response = client
        .get(format!(
            "{}/v1/occurrences/{reference}",
            cloud.trim_end_matches('/')
        ))
        .bearer_auth(&key)
        .send()
        .await
        .with_context(|| format!("downloading occurrence `{reference}` from Cloud"))?;
    let status = response.status();
    let bytes = bounded_cloud_body(response).await?;
    if !status.is_success() {
        let detail = serde_json::from_slice::<serde_json::Value>(&bytes)
            .ok()
            .and_then(|value| {
                value
                    .get("error")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_else(|| String::from_utf8_lossy(&bytes).trim().to_string());
        anyhow::bail!("Cloud occurrence download failed ({status}): {detail}");
    }
    let downloaded: CloudOccurrence =
        serde_json::from_slice(&bytes).context("Cloud occurrence response was invalid")?;
    let provenance = match (&downloaded.app_id, &downloaded.bucket_id) {
        (Some(app_id), Some(bucket_id)) => {
            validate_cloud_route_id(app_id, "app id")?;
            validate_cloud_route_id(bucket_id, "bucket id")?;
            Some(CloudProvenance {
                app_id: app_id.clone(),
                bucket_id: bucket_id.clone(),
                cloud_base: cloud.trim_end_matches('/').to_string(),
            })
        }
        (None, None) | (Some(_), None) => None,
        (None, Some(_)) => anyhow::bail!("Cloud occurrence named a bucket without an app"),
    };
    if downloaded.occurrence.occurrence_id != reference {
        anyhow::bail!("Cloud returned a different occurrence identity");
    }
    downloaded.occurrence.validate().map_err(protocol_error)?;
    downloaded
        .assessment
        .validate(&downloaded.occurrence)
        .map_err(protocol_error)?;
    if let Some(package) = &downloaded.package {
        package.validate().map_err(protocol_error)?;
        if package.occurrence != downloaded.occurrence
            || package.assessment != downloaded.assessment
        {
            anyhow::bail!("Cloud package does not match its immutable occurrence");
        }
    }
    if let Some(readiness) = &downloaded.readiness {
        readiness.validate().map_err(protocol_error)?;
        if readiness.occurrence_id != reference {
            anyhow::bail!("Cloud readiness belongs to a different occurrence");
        }
    }
    if let Some(capture) = &downloaded.capture {
        capture.validate().map_err(protocol_error)?;
        let recomputed = compile_capture_failure(
            capture,
            &downloaded.occurrence.received_at,
            CaptureAssessmentScope::Portable,
        )
        .map_err(protocol_error)?
        .context("Cloud occurrence capture contains no failure")?;
        if recomputed.occurrence != downloaded.occurrence
            || recomputed.assessment != downloaded.assessment
        {
            anyhow::bail!("Cloud occurrence does not match its immutable capture batch");
        }
    }
    if downloaded.package.is_none() && downloaded.capture.is_none() {
        anyhow::bail!("Cloud occurrence has neither a package nor an immutable capture batch");
    }
    let artifacts = match &downloaded.capture {
        Some(capture) => {
            download_cloud_artifacts(&client, cloud.trim_end_matches('/'), &key, capture).await?
        }
        None => Vec::new(),
    };

    let root = std::env::current_dir()?.canonicalize()?;
    let localized = localize_cloud_occurrence(&root, downloaded)?;
    let parent = root.join(".reproit").join("occurrences");
    std::fs::create_dir_all(&parent)?;
    let directory = parent.join(reference);
    if directory.exists() {
        anyhow::bail!(
            "occurrence directory {} appeared while downloading",
            directory.display()
        );
    }
    let staging = parent.join(format!(".{reference}.{}.staging", std::process::id()));
    std::fs::create_dir(&staging)?;
    let persist_result = (|| -> Result<()> {
        if let Some(capture) = &localized.capture {
            write_json_atomically(&staging.join("capture.json"), capture)?;
            // A backend SDK batch projects back to the replayable
            // `reproit-backend-capture` payload; persist it so the occurrence
            // can be re-evaluated offline without an execution plan.
            if let Some(backend) = backend_capture_from_batch(capture) {
                write_json_atomically(&staging.join("backend-capture.json"), &backend)?;
            }
        }
        write_json_atomically(&staging.join("package.json"), &localized.package)?;
        if let Some(provenance) = &provenance {
            write_json_atomically(&staging.join("cloud-provenance.json"), provenance)?;
        }
        if !artifacts.is_empty() {
            let artifact_directory = staging.join("artifacts");
            std::fs::create_dir(&artifact_directory)?;
            for artifact in artifacts {
                write_private_bytes_atomically(
                    &artifact_directory.join(artifact.filename),
                    &artifact.bytes,
                )?;
            }
        }
        std::fs::rename(&staging, &directory)?;
        Ok(())
    })();
    if persist_result.is_err() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    persist_result?;
    ctx.say(format!("Pulled occurrence {reference} from Cloud"));
    Ok(())
}

pub(super) fn read_provenance(directory: &Path) -> Result<Option<CloudProvenance>> {
    let path = directory.join("cloud-provenance.json");
    if !path.is_file() {
        return Ok(None);
    }
    let bytes = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
    let provenance: CloudProvenance =
        serde_json::from_slice(&bytes).with_context(|| format!("parsing {}", path.display()))?;
    validate_cloud_route_id(&provenance.app_id, "app id")?;
    validate_cloud_route_id(&provenance.bucket_id, "bucket id")?;
    validate_cloud_base(&provenance.cloud_base)?;
    Ok(Some(provenance))
}

fn validate_cloud_base(value: &str) -> Result<()> {
    let url = reqwest::Url::parse(value).context("stored Cloud base URL is invalid")?;
    let loopback = matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "::1"));
    if (url.scheme() != "https" && !(url.scheme() == "http" && loopback))
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        anyhow::bail!("stored Cloud base URL is not an HTTPS or loopback origin");
    }
    Ok(())
}

fn validate_cloud_route_id(value: &str, label: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        anyhow::bail!("Cloud occurrence returned an invalid {label}");
    }
    Ok(())
}

async fn download_cloud_artifacts(
    client: &reqwest::Client,
    cloud: &str,
    key: &str,
    capture: &CaptureBatch,
) -> Result<Vec<DownloadedArtifact>> {
    let mut downloaded = Vec::new();
    let mut total_bytes = 0usize;
    for artifact in capture
        .artifacts
        .iter()
        .filter(|artifact| artifact.policy == ArtifactPolicy::Exportable)
    {
        let expected_bytes = usize::try_from(artifact.bytes)
            .context("capture artifact byte length does not fit this platform")?;
        if expected_bytes > MAX_CLOUD_ARTIFACT_BYTES {
            anyhow::bail!("Cloud capture artifact exceeds the 25 MiB download limit");
        }
        total_bytes = total_bytes
            .checked_add(expected_bytes)
            .context("capture artifact aggregate byte length overflow")?;
        if total_bytes > MAX_PLAINTEXT_BYTES {
            anyhow::bail!("Cloud capture artifacts exceed the 64 MiB occurrence limit");
        }
        let mut url = reqwest::Url::parse(cloud).context("Cloud URL is invalid")?;
        url.path_segments_mut()
            .map_err(|_| anyhow::anyhow!("Cloud URL cannot accept path segments"))?
            .extend([
                "v1",
                "capture-artifacts",
                capture.project_id.as_str(),
                artifact.id.as_str(),
            ]);
        let response = client
            .get(url)
            .bearer_auth(key)
            .send()
            .await
            .with_context(|| format!("downloading capture artifact {}", artifact.id))?;
        let status = response.status();
        if !status.is_success() {
            anyhow::bail!(
                "Cloud capture artifact {} download failed ({status})",
                artifact.id
            );
        }
        let bytes = bounded_artifact_body(response, expected_bytes).await?;
        if bytes.len() != expected_bytes || format!("sha256:{}", sha256_hex(&bytes)) != artifact.id
        {
            anyhow::bail!(
                "Cloud capture artifact {} failed integrity verification",
                artifact.id
            );
        }
        downloaded.push(DownloadedArtifact {
            filename: artifact.id[7..].to_string(),
            bytes,
        });
    }
    Ok(downloaded)
}

async fn bounded_artifact_body(
    mut response: reqwest::Response,
    expected_bytes: usize,
) -> Result<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|length| length != expected_bytes as u64)
    {
        anyhow::bail!("Cloud capture artifact length did not match metadata");
    }
    let mut bytes = Vec::with_capacity(expected_bytes);
    while let Some(chunk) = response.chunk().await? {
        if bytes.len().saturating_add(chunk.len()) > expected_bytes {
            anyhow::bail!("Cloud capture artifact exceeded its declared length");
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn localize_cloud_occurrence(
    root: &Path,
    downloaded: CloudOccurrence,
) -> Result<LocalizedOccurrence> {
    let CloudOccurrence {
        occurrence,
        mut assessment,
        package,
        capture,
        ..
    } = downloaded;
    let package = if let Some(package) = package {
        package
    } else if assessment.status == AssessmentStatus::Eligible {
        match crate::adapters::execution::compile_automatic_package(
            root,
            occurrence.clone(),
            assessment.clone(),
        ) {
            Ok(package) => package,
            Err(error) => {
                assessment.status = AssessmentStatus::Incomplete;
                assessment.unresolved = assessment
                    .requirements
                    .iter()
                    .filter(|requirement| requirement.level == RequirementLevel::Required)
                    .map(|requirement| UnresolvedRequirement {
                        requirement_id: requirement.id.clone(),
                        reason: UnresolvedRequirementReason::AmbiguousMapping,
                        detail: format!("trusted local provider resolution failed: {error}"),
                    })
                    .collect();
                incomplete_download_package(occurrence, assessment)?
            }
        }
    } else {
        incomplete_download_package(occurrence, assessment)?
    };
    Ok(LocalizedOccurrence { package, capture })
}

fn incomplete_download_package(
    occurrence: OccurrenceEnvelope,
    assessment: CapabilityAssessment,
) -> Result<ReproductionPackage> {
    let mut package = ReproductionPackage {
        version: PACKAGE_VERSION,
        id: String::new(),
        occurrence,
        assessment,
        plan: None,
        capsule: None,
        legacy: None,
    };
    package.finalize_id().map_err(protocol_error)?;
    package.validate().map_err(protocol_error)?;
    Ok(package)
}

async fn bounded_cloud_body(mut response: reqwest::Response) -> Result<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_CLOUD_RESPONSE_BYTES as u64)
    {
        anyhow::bail!("Cloud occurrence response exceeded 4 MiB");
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if bytes.len().saturating_add(chunk.len()) > MAX_CLOUD_RESPONSE_BYTES {
            anyhow::bail!("Cloud occurrence response exceeded 4 MiB");
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn validate_cloud_occurrence_id(reference: &str) -> Result<()> {
    if reference.starts_with("occ_")
        && reference.len() <= 128
        && reference
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Ok(());
    }
    anyhow::bail!("invalid occurrence id `{reference}`")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_cloud_origin_cannot_redirect_credentials() {
        assert!(validate_cloud_base("https://cloud.reproit.com").is_ok());
        assert!(validate_cloud_base("http://127.0.0.1:8080").is_ok());
        assert!(validate_cloud_base("http://attacker.example").is_err());
        assert!(validate_cloud_base("https://user:secret@example.com").is_err());
    }
}
