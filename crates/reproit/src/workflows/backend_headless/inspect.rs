//! Interactive, human-paced inspection of one backend reproduction.
//!
//! The backend counterpart of the UI `reproit inspect` contract: the verdict
//! comes only from the unchanged `backend::evaluate` engine over the
//! accumulated event sequence, inspection writes transcript evidence but never
//! updates or promotes any saved guard, and the command is interactive only.
//!
//! Two modes. LIVE (default) attaches to the configured backend target the
//! same way replay does and re-sends the recorded operation sequence one step
//! at a time; re-sending recorded mutating requests is a replay, never fuzzing
//! or new input generation. TRACE STEPPING (`--offline`, or the automatic
//! fallback when no live target is configured or reachable) steps through the
//! recorded events only.
//!
//! Pacing reads line commands from stdin (Enter = next step, C = continue to
//! failure, Q = stop); end-of-input continues to failure, so scripted runs can
//! pipe newlines.

use super::inspect_plan::{
    attach_capture_target, plan_capture_steps, plan_finding_steps, probe_target, resolve_source,
    Expected, PlannedStep, Source,
};
use super::inspect_plan::{origin_url, recorded_input, recorded_return};
use super::inspect_report::{
    diff_effects, effect_identities, pending_delta, MemberOutcome, Transcript,
};
use super::replay::apply_request_bindings;
use super::transport::{invoke_traced, AdapterTrail, InspectTrace};
use super::*;
use crate::domain::backend::{pending_obligations, PendingObligation};
use crate::workflows::backend_target;
use std::io::Write as _;

/// Route `reproit inspect` to the backend path when the reference is a
/// captured-production payload file, a backend finding, or a capture-bearing
/// production bucket on a backend-configured project. Returns `None` when the
/// reference belongs to the UI inspection path.
pub async fn try_inspect(
    ctx: &Ctx,
    config_path: Option<&Path>,
    raw_reference: &str,
    offline: bool,
) -> Result<Option<ExitCode>> {
    let reference = raw_reference.strip_prefix('@').unwrap_or(raw_reference);
    let source = match resolve_source(ctx, config_path, reference).await? {
        Some(source) => source,
        None => return Ok(None),
    };
    run_inspection(ctx, config_path, reference, source, offline)
        .await
        .map(Some)
}

async fn run_inspection(
    ctx: &Ctx,
    config_path: Option<&Path>,
    reference: &str,
    source: Source,
    offline: bool,
) -> Result<ExitCode> {
    let root = std::env::current_dir()?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::limited(3))
        .build()?;
    match source {
        Source::Capture { label, artifact } => {
            inspect_capture(
                ctx,
                config_path,
                &root,
                &client,
                reference,
                label,
                artifact,
                offline,
            )
            .await
        }
        Source::Finding { id, artifact } => {
            if offline {
                bail!(
                    "backend finding artifacts carry the requests to re-send but no recorded \
                     event trail; offline stepping needs a captured-production payload file"
                );
            }
            inspect_finding(ctx, &root, &client, &id, &artifact).await
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn inspect_capture(
    ctx: &Ctx,
    config_path: Option<&Path>,
    root: &Path,
    client: &reqwest::Client,
    reference: &str,
    label: String,
    artifact: capture_replay::CaptureArtifact,
    offline: bool,
) -> Result<ExitCode> {
    let mut events = artifact.events;
    events.sort_by_key(|event| event.sequence);
    let configured = backend_target::resolve(config_path)?;
    let proofs = configured
        .as_ref()
        .map(|(_, config)| config.proofs.clone())
        .unwrap_or_default();
    let mut steps = plan_capture_steps(&events, &proofs)?;
    // The capture is evaluated under the same synthesized declared contracts
    // `debug replay-capture` uses, so the verdict is identical; authored
    // policy from reproit.yaml (invariants, resources, proofs, fleet)
    // additionally participates when present.
    let mut config = BackendConfig {
        enabled: true,
        operations: events
            .iter()
            .map(|event| event.operation.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .map(capture_replay::capture_contract)
            .collect(),
        ..BackendConfig::default()
    };
    if let Some((_, declared)) = &configured {
        config.invariants = declared.invariants.clone();
        config.resources = declared.resources.clone();
        config.proofs = declared.proofs.clone();
        config.fleet = declared.fleet.clone();
    }
    let expected = Expected::CaptureTarget {
        operation: artifact.operation,
        oracle: artifact.oracle,
    };

    let mut live = false;
    if offline {
        ctx.say("Stepping the recorded capture offline (--offline).");
    } else {
        match attach_capture_target(client, config_path, &mut steps).await? {
            Ok(base_url) => {
                maybe_reset_target(client, &base_url).await?;
                ctx.say(format!(
                    "Re-sending the captured sequence against the configured backend target \
                     {base_url}."
                ));
                live = true;
            }
            Err(reason) => {
                ctx.say(format!("{reason}; stepping the recorded trace offline."));
            }
        }
    }
    let session = InspectSession {
        mode: if live { "live" } else { "offline" },
        source: label,
        reference: reference.to_string(),
        config,
        expected,
        steps,
        recorded_stream: events,
        finding: false,
    };
    drive(ctx, root, client, session).await
}

async fn inspect_finding(
    ctx: &Ctx,
    root: &Path,
    client: &reqwest::Client,
    id: &str,
    artifact: &BackendFindingArtifact,
) -> Result<ExitCode> {
    if std::env::var_os("REPROIT_BACKEND_RESET_URL").is_none() {
        if let Some(reset_url) = &artifact.reset_url {
            std::env::set_var("REPROIT_BACKEND_RESET_URL", reset_url);
        }
    }
    let fingerprint = artifact
        .finding
        .get("fingerprint")
        .and_then(Value::as_str)
        .context("backend artifact has no finding fingerprint")?
        .to_string();
    let mut operations: Vec<OperationContract> = artifact
        .setup
        .iter()
        .map(|step| step.contract.clone())
        .collect();
    operations.push(artifact.failing.contract.clone());
    let config = BackendConfig {
        enabled: true,
        operations,
        invariants: artifact.failing.policy.invariants.clone(),
        resources: artifact.failing.policy.resources.clone(),
        proofs: artifact.failing.policy.proofs.clone(),
        fleet: artifact.failing.policy.fleet.clone(),
        ..BackendConfig::default()
    };
    let steps = plan_finding_steps(artifact)?;
    if !probe_target(client, &origin_url(&artifact.failing.request.url)?).await {
        bail!(
            "the saved target for {id} ({}) is not reachable; finding inspection re-sends the \
             saved requests and has no recorded trail to step offline",
            artifact.failing.request.url
        );
    }
    maybe_reset_target(client, &artifact.failing.request.url).await?;
    ctx.say(format!(
        "Re-sending the saved reproduction of {id} against {}.",
        artifact.failing.request.url
    ));
    let session = InspectSession {
        mode: "live",
        source: format!("backend finding {id}"),
        reference: id.to_string(),
        config,
        expected: Expected::Fingerprint(fingerprint),
        steps,
        recorded_stream: Vec::new(),
        finding: true,
    };
    drive(ctx, root, client, session).await
}

struct InspectSession {
    mode: &'static str,
    source: String,
    reference: String,
    config: BackendConfig,
    expected: Expected,
    steps: Vec<PlannedStep>,
    /// The full recorded stream, revealed prefix-wise in offline mode so the
    /// final sequence is byte-identical to what `debug replay-capture` sees.
    recorded_stream: Vec<BackendEvent>,
    finding: bool,
}

enum Decision {
    Step,
    Continue,
    Abort,
}

fn prompt_step(step: &PlannedStep, index: usize, total: usize) -> Decision {
    println!("\ninspect [backend] {index}/{total}: {}", step.label);
    for member in &step.members {
        if let Some(live) = &member.live {
            println!("  will send: {} {}", live.request.method, live.request.url);
        }
    }
    print!("  Enter: run this step, C: continue to failure, Q: stop > ");
    let _ = std::io::stdout().flush();
    let mut answer = String::new();
    match std::io::stdin().read_line(&mut answer) {
        Ok(0) => Decision::Continue, // end of input: no operator is pacing.
        Ok(_) => match answer.trim().to_ascii_lowercase().as_str() {
            "c" | "continue" => Decision::Continue,
            "q" | "quit" | "stop" => Decision::Abort,
            _ => Decision::Step,
        },
        Err(_) => Decision::Abort,
    }
}

async fn drive(
    ctx: &Ctx,
    root: &Path,
    client: &reqwest::Client,
    session: InspectSession,
) -> Result<ExitCode> {
    ctx.say("Use Enter to run the next step or C to continue to failure.");
    let total = session.steps.len();
    let mut transcript = Transcript::new(
        session.mode,
        session.source.clone(),
        session.reference.clone(),
        session.expected.describe(),
    );
    let mut accumulated: Vec<BackendEvent> = Vec::new();
    let mut outputs: Vec<Value> = Vec::new();
    let mut pending: Vec<PendingObligation> = Vec::new();
    let mut revealed = 0usize;
    let mut continue_mode = false;
    let mut verdict: Option<String> = None;
    let mut reproduced = false;
    for (position, step) in session.steps.iter().enumerate() {
        let index = position + 1;
        if !continue_mode {
            match prompt_step(step, index, total) {
                Decision::Step => {}
                Decision::Continue => continue_mode = true,
                Decision::Abort => {
                    verdict = Some(format!("stopped by the operator before step {index}"));
                    break;
                }
            }
        }
        let execution = if session.mode == "live" {
            execute_live_step(
                client,
                position,
                step,
                &mut accumulated,
                &mut outputs,
                session.finding,
            )
            .await?
        } else {
            reveal_recorded_step(
                &session.recorded_stream,
                step,
                &mut revealed,
                &mut accumulated,
            )
        };
        let violations = backend::evaluate(&session.config, &accumulated);
        let now_pending = pending_obligations(&session.config, &accumulated);
        let delta = pending_delta(&pending, &now_pending);
        pending = now_pending;
        let fired = violations
            .iter()
            .find(|violation| session.expected.matches(violation))
            .cloned();
        display_step(
            ctx,
            index,
            total,
            step,
            &execution.members,
            &delta,
            &pending,
            continue_mode,
        );
        transcript.record_step(
            index,
            total,
            &step.label,
            step.grouped,
            &execution.members,
            &pending,
            &delta,
            &violations,
            fired.as_ref(),
        );
        if let Some(violation) = fired {
            ctx.say(format!(
                "\nVIOLATION at step {index}: {} on `{}`\n  {}\n  fingerprint {}",
                backend::finding(&violation)["oracle"]
                    .as_str()
                    .unwrap_or(&violation.oracle),
                violation.operation,
                violation.reason,
                violation.fingerprint
            ));
            verdict = Some(format!("reproduced at step {index} of {total}"));
            reproduced = true;
            break;
        }
        if let Some(halt) = execution.halt {
            ctx.say(format!("\n{halt}"));
            verdict = Some(halt);
            break;
        }
    }
    let verdict = verdict.unwrap_or_else(|| {
        format!(
            "completed all {total} step(s); the expected violation ({}) did not fire",
            session.expected.describe()
        )
    });
    if !reproduced {
        ctx.say(format!("\n{verdict}"));
    }
    let (markdown_path, json_path) = transcript.write(root, &verdict, reproduced)?;
    ctx.say("\nInspection transcript:".to_string());
    ctx.say(format!("  human:   {}", markdown_path.display()));
    ctx.say(format!("  agent:   {}", json_path.display()));
    Ok(if reproduced {
        Exit::Regression.code()
    } else {
        ExitCode::SUCCESS
    })
}

struct StepExecution {
    members: Vec<MemberOutcome>,
    /// Set when the session cannot meaningfully continue (a finding setup
    /// step that did not replay cleanly, or a binding that no longer applies).
    halt: Option<String>,
}

async fn execute_live_step(
    client: &reqwest::Client,
    position: usize,
    step: &PlannedStep,
    accumulated: &mut Vec<BackendEvent>,
    outputs: &mut Vec<Value>,
    finding: bool,
) -> Result<StepExecution> {
    let mut prepared = Vec::new();
    for (member_index, member) in step.members.iter().enumerate() {
        let live = member
            .live
            .as_ref()
            .expect("live mode plans a request for every member");
        let mut request = live.request.clone();
        if finding && !apply_request_bindings(&mut request, outputs) {
            return Ok(StepExecution {
                members: Vec::new(),
                halt: Some(format!(
                    "a saved value binding for `{}` no longer applies; the reproduction is \
                     incomplete against the current target state (not reproduced)",
                    member.operation
                )),
            });
        }
        prepared.push((member_index, member, live, request));
    }
    // Grouped members are sent concurrently so multi-actor interleavings keep
    // their shape; single members degrade to a sequential send.
    let sends = prepared.iter().map(|(member_index, _, live, request)| {
        invoke_traced(
            client,
            &live.endpoint,
            request.clone(),
            Some(InspectTrace {
                trace_id: format!("reproit-inspect-{position}-{member_index}"),
                action_index: (position + 1) as u32,
            }),
        )
    });
    let results = futures_util::future::join_all(sends).await;
    let mut members = Vec::new();
    let mut halt = None;
    let mut staged: Vec<(u64, BackendEvent)> = Vec::new();
    for ((member_index, member, live, request), sent) in prepared.iter().zip(results) {
        let (result, trail) = sent?;
        outputs.push(result.output.clone());
        let (trail_events, trail_note) = match trail {
            AdapterTrail::Events(events) if !events.is_empty() => (Some(events), None),
            AdapterTrail::Events(_) | AdapterTrail::Absent => (
                None,
                Some("no x-reproit-events trail (target not instrumented?)".to_string()),
            ),
            AdapterTrail::Malformed => (
                None,
                Some("x-reproit-events header was unreadable; response evidence only".to_string()),
            ),
        };
        // Findings keep exact replay parity by evaluating the synthesized
        // request/response events; captures prefer the adapter trail, which
        // carries the effects and completeness proofs the oracles need.
        let synthesized = trail_events.is_none() || finding;
        let eval_events = if synthesized {
            result.events
        } else {
            trail_events.clone().expect("checked above")
        };
        let observed = trail_events
            .as_deref()
            .map(effect_identities)
            .unwrap_or_default();
        let diff = (!member.recorded.is_empty())
            .then(|| {
                trail_events
                    .as_deref()
                    .map(|live| diff_effects(&member.recorded, live))
            })
            .flatten();
        if live.setup && (!(200..400).contains(&result.status) || !result.violations.is_empty()) {
            halt = Some(format!(
                "setup step `{}` returned {} and did not replay cleanly; the saved failing \
                 request was not sent (not reproduced)",
                member.operation, result.status
            ));
        }
        for mut event in eval_events {
            let original = event.sequence;
            if synthesized {
                event.trace_id = "reproit-inspect".into();
                event.span_id = format!("step-{position}-m{member_index}");
            }
            event.action_index = (position + 1) as u32;
            staged.push((original, event));
        }
        members.push(MemberOutcome {
            operation: member.operation.clone(),
            request_line: format!("{} {}", request.method, request.url),
            input: request.input.clone(),
            status: Some(result.status),
            output: result.output.clone(),
            observed_effects: observed,
            diff,
            trail_note,
        });
        if halt.is_some() {
            break;
        }
    }
    // Interleave grouped members by the adapter's original sequence numbers,
    // then renumber into the session stream.
    staged.sort_by_key(|(original, _)| *original);
    for (_, mut event) in staged {
        event.sequence = accumulated.len() as u64 + 1;
        accumulated.push(event);
    }
    Ok(StepExecution { members, halt })
}

fn reveal_recorded_step(
    stream: &[BackendEvent],
    step: &PlannedStep,
    revealed: &mut usize,
    accumulated: &mut Vec<BackendEvent>,
) -> StepExecution {
    let cut = step.cut.min(stream.len());
    accumulated.extend(stream[*revealed..cut].iter().cloned());
    *revealed = cut;
    let members = step
        .members
        .iter()
        .map(|member| {
            let (status, output) = recorded_return(&member.recorded);
            MemberOutcome {
                operation: member.operation.clone(),
                request_line: "recorded invocation".into(),
                input: recorded_input(&member.recorded)
                    .cloned()
                    .unwrap_or(Value::Null),
                status,
                output,
                observed_effects: effect_identities(&member.recorded),
                diff: None,
                trail_note: None,
            }
        })
        .collect();
    StepExecution {
        members,
        halt: None,
    }
}

#[allow(clippy::too_many_arguments)]
fn display_step(
    ctx: &Ctx,
    index: usize,
    total: usize,
    step: &PlannedStep,
    members: &[MemberOutcome],
    delta: &(Vec<String>, Vec<String>),
    pending: &[PendingObligation],
    continue_mode: bool,
) {
    if continue_mode {
        let statuses = members
            .iter()
            .map(|member| {
                member
                    .status
                    .map_or("(no status)".into(), |status| status.to_string())
            })
            .collect::<Vec<_>>()
            .join(", ");
        ctx.say(format!("  {index}/{total} {}: {statuses}", step.label));
        return;
    }
    for member in members {
        ctx.say(format!("  -> {}", member.request_line));
        ctx.say(format!("     input:    {}", preview(&member.input)));
        ctx.say(format!(
            "  <- {} {}",
            member
                .status
                .map_or("(no status)".into(), |status| status.to_string()),
            preview(&member.output)
        ));
        if !member.observed_effects.is_empty() {
            ctx.say(format!(
                "     effects:  {}",
                member.observed_effects.join(", ")
            ));
        }
        if let Some(diff) = &member.diff {
            for (name, items) in [
                ("matched", &diff.matched),
                ("missing", &diff.missing),
                ("unexpected", &diff.unexpected),
            ] {
                if !items.is_empty() {
                    ctx.say(format!("     {name}:  {}", items.join(", ")));
                }
            }
        }
        if let Some(note) = &member.trail_note {
            ctx.say(format!("     trail:    {note}"));
        }
    }
    for added in &delta.0 {
        ctx.say(format!("  oracle state + {added}"));
    }
    for resolved in &delta.1 {
        ctx.say(format!("  oracle state - {resolved}"));
    }
    if delta.0.is_empty() && delta.1.is_empty() && pending.is_empty() {
        ctx.say("  oracle state: no predicates mid-accumulation".to_string());
    }
}

fn preview(value: &Value) -> String {
    let text = value.to_string();
    let mut shown: String = text.chars().take(160).collect();
    if text.chars().count() > 160 {
        shown.push_str("...");
    }
    shown
}
