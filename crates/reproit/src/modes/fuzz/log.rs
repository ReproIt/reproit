use super::*;

fn clean_runner_line(line: &str) -> &str {
    line.trim_start_matches("flutter: ").trim()
}

/// Split a batched drive log into per-seed `(seed, log_slice)` pairs by the
/// `SEED:BEGIN <seed>` ... `SEED:END <seed>` boundary markers the explorer
/// emits. For a single-seed run with no markers, the whole log is returned
/// under that one seed.
pub(super) fn split_seed_segments<'a>(log: &'a str, plans: &[SeedPlan]) -> Vec<(u64, &'a str)> {
    if plans.len() == 1 {
        return vec![(plans[0].seed, log)];
    }
    let mut out = Vec::new();
    let mut current: Option<(u64, usize)> = None;
    let mut offset = 0;
    for chunk in log.split_inclusive('\n') {
        let line = chunk.trim_end_matches(['\r', '\n']);
        if let Some(seed) = marker_seed(line, "SEED:BEGIN ") {
            // Flush any unterminated previous segment defensively.
            if let Some((previous_seed, start)) = current.take() {
                out.push((previous_seed, segment(log, start, offset)));
            }
            current = Some((seed, offset + chunk.len()));
        } else if marker_seed(line, "SEED:END ").is_some() {
            if let Some((seed, start)) = current.take() {
                out.push((seed, segment(log, start, offset)));
            }
        }
        offset += chunk.len();
    }
    if let Some((seed, start)) = current.take() {
        out.push((seed, segment(log, start, log.len())));
    }
    // If the markers were absent, fall back to
    // attributing the whole log to each planned seed so nothing is dropped.
    if out.is_empty() {
        return plans.iter().map(|plan| (plan.seed, log)).collect();
    }
    out
}

/// Split a batched drive log into one segment per `SEED:BEGIN`/`SEED:END` pair,
/// in order, WITHOUT needing the seed plans (the caller knows how many entries
/// it wrote). Used by `check` to batch a repro's N repeat-replays into a single
/// drive (one browser launch) and still read a per-replay verdict. An unmarked
/// log returns the whole log as one segment.
pub(crate) fn split_log_segments(log: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut current = None;
    let mut offset = 0;
    for chunk in log.split_inclusive('\n') {
        let line = chunk.trim_end_matches(['\r', '\n']);
        if marker_seed(line, "SEED:BEGIN ").is_some() {
            if let Some(start) = current.take() {
                out.push(segment(log, start, offset));
            }
            current = Some(offset + chunk.len());
        } else if marker_seed(line, "SEED:END ").is_some() {
            if let Some(start) = current.take() {
                out.push(segment(log, start, offset));
            }
        }
        offset += chunk.len();
    }
    if let Some(start) = current.take() {
        out.push(segment(log, start, log.len()));
    }
    if out.is_empty() {
        return vec![log];
    }
    out
}

fn segment(log: &str, start: usize, end: usize) -> &str {
    log[start..end].trim_end_matches(['\r', '\n'])
}

/// Parse `<prefix><number>` -> the seed, if the line carries the marker.
pub(super) fn marker_seed(line: &str, prefix: &str) -> Option<u64> {
    let i = line.find(prefix)?;
    line[i + prefix.len()..]
        .split_whitespace()
        .next()?
        .parse::<u64>()
        .ok()
}

/// The performed action sequence, from FUZZ:ACT lines in a log slice.
pub(super) fn trace_in_log(log: &str) -> Vec<String> {
    log.lines()
        .filter_map(|l| {
            l.find("FUZZ:ACT ")
                .map(|i| l[i + "FUZZ:ACT ".len()..].trim().to_string())
        })
        .collect()
}

/// App exception findings parsed directly from a drive-log SLICE (one seed's
/// segment of a batched session). Mirrors `app_exceptions` but works on the
/// per-seed text so findings are attributed to the right seed. Captures each
/// "EXCEPTION CAUGHT BY ..." block (excluding the test framework's own) up to
/// the closing ═ rule, pulling kind / message / Dart source frames.
pub(super) fn exceptions_in_log(log: &str) -> Vec<Value> {
    let mut out = Vec::new();
    let mut buf: Option<Vec<&str>> = None;
    for raw in log.lines() {
        if raw.contains("EXCEPTION CAUGHT BY") {
            // Flush an unterminated previous block defensively.
            if let Some(b) = buf.take() {
                if let Some(rec) = exception_record(&b) {
                    out.push(rec);
                }
            }
            buf = Some(vec![raw]);
            continue;
        }
        if let Some(b) = buf.as_mut() {
            let trimmed = clean_runner_line(raw);
            let is_close = !trimmed.is_empty() && trimmed.chars().all(|c| c == '═');
            if is_close || b.len() > 300 {
                if let Some(rec) = exception_record(b) {
                    out.push(rec);
                }
                buf = None;
            } else {
                b.push(raw);
            }
        }
    }
    if let Some(b) = buf {
        if let Some(rec) = exception_record(&b) {
            out.push(rec);
        }
    }
    out
}

/// Turn one captured exception block into a finding Value, or None if it is the
/// test framework's own exception (not an app bug).
fn exception_record(buf: &[&str]) -> Option<Value> {
    let kind = buf
        .first()
        .and_then(|l| {
            let line = clean_runner_line(l);
            let start = line.find('╡')? + '╡'.len_utf8();
            let end = line.find('╞')?;
            Some(line[start..end].trim().to_string())
        })
        .unwrap_or_else(|| "EXCEPTION".to_string());
    if kind.contains("TEST FRAMEWORK") {
        return None;
    }
    let mut message = String::new();
    if let Some(start) = buf
        .iter()
        .position(|line| clean_runner_line(line).starts_with("The following"))
    {
        for raw in &buf[start + 1..] {
            let line = clean_runner_line(raw);
            if line.is_empty() {
                break;
            }
            if !message.is_empty() {
                message.push(' ');
            }
            message.push_str(line);
        }
    }
    let frames: Vec<String> = buf
        .iter()
        .map(|line| clean_runner_line(line))
        .filter(|line| {
            line.contains(".dart") && (line.contains("package:") || line.contains("file://"))
        })
        .take(12)
        .map(str::to_string)
        .collect();
    Some(json!({ "kind": kind, "message": message, "frames": frames }))
}
