use super::target::Target;
use serde_json::Value;

/// One device entry shown in the interactive picker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Device {
    pub name: String,
    pub id: String,
    pub target: Target,
    pub booted: bool,
}

/// Parse `flutter devices --machine` JSON into device entries. The shape is a
/// JSON array of objects with `name`, `id`, and `targetPlatform` (e.g.
/// `ios`, `android-arm64`, `web-javascript`). Best-effort: malformed input
/// yields an empty list.
pub fn parse_flutter_devices(json: &str) -> Vec<Device> {
    let Ok(v) = serde_json::from_str::<Value>(json) else {
        return Vec::new();
    };
    let Some(arr) = v.as_array() else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|d| {
            let name = d.get("name").and_then(Value::as_str)?.to_string();
            let id = d.get("id").and_then(Value::as_str)?.to_string();
            let plat = d
                .get("targetPlatform")
                .and_then(Value::as_str)
                .unwrap_or("");
            let target = if plat.starts_with("ios") {
                Target::Ios
            } else if plat.starts_with("android") {
                Target::Android
            } else if plat.contains("web") {
                Target::Web
            } else {
                return None;
            };
            Some(Device {
                name,
                id,
                target,
                booted: true,
            })
        })
        .collect()
}

/// Parse `xcrun simctl list devices` plain-text output into iOS device entries.
/// Lines under a runtime header look like `    iPhone 16 (UDID) (Booted)`;
/// unavailable devices (`(unavailable, ...)`) are skipped.
pub fn parse_simctl_devices(text: &str) -> Vec<Device> {
    let re = regex::Regex::new(
        r"[0-9A-Fa-f]{8}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{12}",
    )
    .unwrap();
    let mut out = Vec::new();
    // simctl groups devices under runtime headers ("-- iOS 18.0 --", "-- tvOS
    // ... --", "-- watchOS ... --", "-- visionOS ... --"). Only iOS-runtime
    // devices (iPhone/iPad) are valid app-fuzzing targets; skip the TV / Watch /
    // Vision form factors so the picker is not a wall of irrelevant sims.
    let mut in_ios = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("--") {
            in_ios = trimmed.starts_with("-- iOS");
            continue;
        }
        // Skip the "== Devices ==" banner, blanks, and any non-iOS runtime.
        if trimmed.is_empty() || trimmed.starts_with("==") || !in_ios {
            continue;
        }
        if trimmed.contains("(unavailable") {
            continue;
        }
        let Some(m) = re.find(trimmed) else {
            continue;
        };
        let udid = m.as_str().to_string();
        let name = trimmed[..trimmed.find(" (").unwrap_or(trimmed.len())]
            .trim()
            .to_string();
        if name.is_empty() {
            continue;
        }
        out.push(Device {
            name,
            id: udid,
            target: Target::Ios,
            booted: trimmed.contains("(Booted)"),
        });
    }
    out
}

/// Parse `adb devices` plain-text output into Android device entries. The
/// format is a header line `List of devices attached` then `serial\tstate`
/// lines; only `device`-state entries are returned (offline/unauthorized are
/// skipped).
pub fn parse_adb_devices(text: &str) -> Vec<Device> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("List of devices") || line.starts_with('*') {
            continue;
        }
        let mut parts = line.split_whitespace();
        let (Some(serial), Some(state)) = (parts.next(), parts.next()) else {
            continue;
        };
        if state != "device" {
            continue;
        }
        out.push(Device {
            name: serial.to_string(),
            id: serial.to_string(),
            target: Target::Android,
            booted: true,
        });
    }
    out
}
