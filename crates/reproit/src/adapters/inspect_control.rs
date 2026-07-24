//! Platform-runner side of the inspection control protocol.

use anyhow::{bail, Context, Result};
use serde_json::json;
use std::path::PathBuf;
use std::time::{Duration, Instant};

pub(crate) fn pause(
    action: &str,
    step: usize,
    total: usize,
    target: Option<&str>,
    state: Option<&str>,
) -> Result<bool> {
    let Some(dir) = control_dir() else {
        return Ok(true);
    };
    pause_in_dir(&dir, action, step, total, target, state, wait_duration())
}

fn pause_in_dir(
    dir: &std::path::Path,
    action: &str,
    step: usize,
    total: usize,
    target: Option<&str>,
    state: Option<&str>,
    wait: Duration,
) -> Result<bool> {
    std::fs::create_dir_all(dir)?;
    let request = json!({
        "sequence": step,
        "step": step,
        "total": total,
        "action": action,
        "target": target,
        "state": state,
    });
    let temp = dir.join(format!("request-{}.tmp", std::process::id()));
    std::fs::write(&temp, request.to_string())?;
    let _ = std::fs::remove_file(dir.join("request.json"));
    std::fs::rename(temp, dir.join("request.json"))?;

    let deadline = Instant::now() + wait;
    while Instant::now() < deadline {
        if let Ok(raw) = std::fs::read_to_string(dir.join("response.json")) {
            if let Ok(response) = serde_json::from_str::<serde_json::Value>(&raw) {
                if response["sequence"].as_u64() == Some(step as u64) {
                    return match response["decision"].as_str() {
                        Some("continue") => Ok(true),
                        Some("step") => Ok(false),
                        Some("abort") => bail!("inspection stopped by user"),
                        _ => Ok(false),
                    };
                }
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    bail!("inspection timed out while waiting at step {step}")
}

fn control_dir() -> Option<PathBuf> {
    std::env::var_os("REPROIT_INSPECT_CONTROL")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn wait_duration() -> Duration {
    let millis = clamped_wait_millis(std::env::var("REPROIT_INSPECT_WAIT_MS").ok().as_deref());
    Duration::from_millis(millis)
}

fn clamped_wait_millis(raw: Option<&str>) -> u64 {
    raw.and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(240_000)
        .clamp(1_000, 900_000)
}

pub(crate) fn pause_or_context(
    action: &str,
    step: usize,
    total: usize,
    target: Option<&str>,
    state: Option<&str>,
) -> Result<bool> {
    pause(action, step, total, target, state)
        .with_context(|| format!("waiting to inspect action {step}/{total}"))
}

#[cfg(any(target_os = "windows", test))]
pub(crate) fn gate_replay_action(
    action: &str,
    index: usize,
    replay: Option<&[String]>,
    auto_continue: &mut bool,
) -> Result<()> {
    let Some(replay) = replay.filter(|_| !*auto_continue) else {
        return Ok(());
    };
    *auto_continue = pause_or_context(action, index + 1, replay.len(), Some(action), None)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wait_is_bounded() {
        assert_eq!(clamped_wait_millis(Some("1")), 1_000);
        assert_eq!(clamped_wait_millis(Some("5000")), 5_000);
        assert_eq!(clamped_wait_millis(Some("9999999")), 900_000);
        assert_eq!(clamped_wait_millis(Some("nope")), 240_000);
    }

    #[test]
    fn step_response_releases_the_matching_request() {
        let dir = std::env::temp_dir().join(format!("reproit-inspect-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("response.json"),
            r#"{"sequence":2,"decision":"step"}"#,
        )
        .unwrap();

        let auto_continue = pause_in_dir(
            &dir,
            "tap:key:checkout",
            2,
            3,
            Some("Checkout"),
            None,
            Duration::from_secs(1),
        )
        .unwrap();

        assert!(!auto_continue);
        let request: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("request.json")).unwrap())
                .unwrap();
        assert_eq!(request["action"], "tap:key:checkout");
        assert_eq!(request["target"], "Checkout");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn replay_gate_skips_non_replay_and_auto_continue_actions() {
        let mut auto_continue = false;
        gate_replay_action("tap:key:checkout", 0, None, &mut auto_continue).unwrap();
        assert!(!auto_continue);

        auto_continue = true;
        gate_replay_action(
            "tap:key:checkout",
            0,
            Some(&["tap:key:checkout".to_string()]),
            &mut auto_continue,
        )
        .unwrap();
        assert!(auto_continue);
    }
}
