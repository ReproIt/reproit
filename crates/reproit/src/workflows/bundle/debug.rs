use super::{cloud, find_occurrence, protocol_error, write_json_atomically};
use crate::adapters::execution::{self, AutomaticCompilation};
use crate::interface::cli::context::Ctx;
use crate::workflows::triage;
use anyhow::{Context, Result};
use reproit_protocol::{AssessmentStatus, DiagnosticReceipt, ReproductionPackage};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

pub(crate) async fn debug_occurrence(
    ctx: &Ctx,
    reference: &str,
    at: &str,
    ide: &str,
    no_open: bool,
) -> Result<ExitCode> {
    validate_options(at, ide)?;
    validate_interaction(ctx)?;
    if find_occurrence(reference).is_none() {
        cloud::pull_cloud_occurrence(ctx, reference).await?;
    }
    let (root, directory) = find_occurrence(reference)
        .with_context(|| format!("no local or Cloud occurrence `{reference}` is available"))?;
    let mut package = read_package(&directory)?;
    if package.assessment.status != AssessmentStatus::Eligible || package.plan.is_none() {
        package = compile_package(&root, &package)?;
    }
    let readiness = execution::assess_package_readiness(&root, &package)?;
    require_debug_ready(&readiness)?;
    write_json_atomically(&directory.join("readiness.json"), &readiness)?;
    ctx.say("Starting a non-authoritative diagnostic executor");
    let run = execution::execute_diagnostic(
        &root,
        &package,
        execution::DebugLaunchOptions {
            ide: ide.to_string(),
            open: !no_open,
        },
    )
    .await?;
    let receipt = run
        .diagnostic_receipt
        .as_ref()
        .context("diagnostic execution omitted its receipt")?;
    let descriptor = debug_descriptor_path(&root, receipt, ide)?;
    let latest = directory.join("latest-diagnostic.json");
    write_json_atomically(&latest, &run)?;
    report_to_cloud(ctx, &directory, &package, &run).await?;
    ctx.emit(&serde_json::json!({
        "command": "debug occurrence",
        "occurrenceId": &package.occurrence.occurrence_id,
        "authoritative": false,
        "run": &run,
        "debugDescriptor": &descriptor,
    }));
    ctx.say(format!("Diagnostic session {}", receipt.receipt_id));
    ctx.say("  verdict: diagnostic only, run the normal occurrence to verify a fix");
    ctx.say(format!("  receipt: {}", latest.display()));
    ctx.say(format!("  debugger: {}", descriptor.display()));
    Ok(ExitCode::SUCCESS)
}

pub(crate) async fn explain_occurrence(ctx: &Ctx, reference: &str) -> Result<ExitCode> {
    if find_occurrence(reference).is_none() {
        cloud::pull_cloud_occurrence(ctx, reference).await?;
    }
    let (root, directory) = find_occurrence(reference)
        .with_context(|| format!("no local or Cloud occurrence `{reference}` is available"))?;
    let package = read_package(&directory)?;
    let (package, blockers) = if package.plan.is_none() {
        match execution::compile_package_automatically(&root, &package)? {
            AutomaticCompilation::Compiled(compiled) => (*compiled, Vec::new()),
            AutomaticCompilation::Blocked(blockers) => (package, blockers),
        }
    } else {
        (package, Vec::new())
    };
    let readiness = execution::assess_package_readiness(&root, &package)?;
    ctx.emit(&serde_json::json!({
        "command": "debug explain",
        "occurrenceId": reference,
        "readiness": readiness,
        "planningBlockers": blockers,
    }));
    if !ctx.json {
        ctx.say(format!("Readiness for {reference}"));
        for dimension in &readiness.dimensions {
            ctx.say(format!(
                "  {:?}: {:?}",
                dimension.dimension, dimension.status
            ));
            for gap in &dimension.gaps {
                ctx.say(format!("    {}: {}", gap.capability, gap.detail));
                if let Some(action) = &gap.next_action {
                    ctx.say(format!("      next: {action}"));
                }
            }
        }
        for claim in &readiness.fidelity {
            ctx.say(format!(
                "  fidelity {}: {:?} ({} -> {})",
                claim.dimension,
                claim.status,
                claim.source_value,
                claim.replay_value.as_deref().unwrap_or("unavailable")
            ));
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn require_debug_ready(readiness: &reproit_protocol::ReadinessAssessment) -> Result<()> {
    use reproit_protocol::{ReadinessDimension, ReadinessStatus};

    let debug = readiness.dimension(ReadinessDimension::Debug);
    if debug.status == ReadinessStatus::Ready {
        return Ok(());
    }
    let detail = debug
        .gaps
        .iter()
        .map(|gap| format!("{}: {}", gap.capability, gap.detail))
        .collect::<Vec<_>>()
        .join("; ");
    anyhow::bail!("occurrence is not debug-ready: {detail}")
}

fn validate_interaction(ctx: &Ctx) -> Result<()> {
    use std::io::IsTerminal;

    if ctx.confirmed() || ctx.json || !std::io::stdin().is_terminal() {
        anyhow::bail!("debug occurrence requires an interactive terminal without --yes or --json");
    }
    Ok(())
}

fn validate_options(at: &str, ide: &str) -> Result<()> {
    if at != "before-trigger" {
        anyhow::bail!(
            "unsupported causal pause point `{at}`; this target supports `before-trigger`"
        );
    }
    if !matches!(ide, "auto" | "vscode" | "json") {
        anyhow::bail!("unsupported IDE format `{ide}`; use `auto`, `vscode`, or `json`");
    }
    Ok(())
}

fn read_package(directory: &Path) -> Result<ReproductionPackage> {
    let path = directory.join("package.json");
    let package: ReproductionPackage = serde_json::from_slice(
        &std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?,
    )
    .with_context(|| format!("parsing {}", path.display()))?;
    package.validate().map_err(protocol_error)?;
    Ok(package)
}

fn compile_package(root: &Path, package: &ReproductionPackage) -> Result<ReproductionPackage> {
    match execution::compile_package_automatically(root, package)? {
        AutomaticCompilation::Compiled(compiled) => Ok(*compiled),
        AutomaticCompilation::Blocked(blockers) => {
            let detail = blockers
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; ");
            anyhow::bail!("occurrence cannot enter a debug session: {detail}");
        }
    }
}

async fn report_to_cloud(
    ctx: &Ctx,
    directory: &Path,
    package: &ReproductionPackage,
    run: &execution::PlanRun,
) -> Result<()> {
    let Some(provenance) = cloud::read_provenance(directory)? else {
        return Ok(());
    };
    let cell_receipt = run
        .cell_receipt
        .as_ref()
        .context("diagnostic execution omitted its cell receipt")?;
    let diagnostic_receipt = run
        .diagnostic_receipt
        .as_ref()
        .context("diagnostic execution omitted its receipt")?;
    match triage::report_diagnostic_session(
        &provenance.cloud_base,
        &provenance.app_id,
        &provenance.bucket_id,
        &package.occurrence.occurrence_id,
        cell_receipt,
        diagnostic_receipt,
    )
    .await
    {
        Ok(()) => ctx.say("  cloud: diagnostic receipt recorded without a verdict"),
        Err(error) => ctx.say(format!(
            "  cloud: diagnostic receipt was not uploaded: {error}"
        )),
    }
    Ok(())
}

fn debug_descriptor_path(root: &Path, receipt: &DiagnosticReceipt, ide: &str) -> Result<PathBuf> {
    let directory = root.join(".reproit").join("cells").join(&receipt.run_id);
    let name = if directory.join("reproit.code-workspace").is_file() {
        "reproit.code-workspace"
    } else if ide == "vscode" && directory.join("launch.json").is_file() {
        "launch.json"
    } else {
        "debug-session.json"
    };
    let path = directory.join(name);
    if !path.is_file() {
        anyhow::bail!(
            "diagnostic execution omitted debugger descriptor {}",
            path.display()
        );
    }
    Ok(path)
}
