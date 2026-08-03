//! Fail-closed decisions shared by native accessibility runners.
//!
//! Platform adapters own acquisition. These helpers only accept observations
//! that carry stable developer keys, native object identity, and complete
//! geometry/focus/scroll evidence. Missing or ambiguous evidence abstains.

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct NativeNode {
    pub(crate) key: String,
    pub(crate) identity: u64,
    pub(crate) role: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct FocusArm {
    pub(crate) key: String,
    pub(crate) identity: u64,
    pub(crate) role: String,
    pub(crate) dialog_count: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct FocusAfter {
    pub(crate) target: NativeNode,
    pub(crate) focused_identity: Option<u64>,
    pub(crate) focus_is_window: bool,
    pub(crate) dialog_count: usize,
    pub(crate) same_screen: bool,
}

pub(crate) fn focus_was_lost(arm: Option<&FocusArm>, after: Option<&FocusAfter>) -> bool {
    let (Some(arm), Some(after)) = (arm, after) else {
        return false;
    };
    arm.role != "link"
        && arm.identity == after.target.identity
        && arm.key == after.target.key
        && arm.dialog_count == after.dialog_count
        && after.same_screen
        && after.focus_is_window
        && after.focused_identity != Some(arm.identity)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ScrollSample {
    pub(crate) offset_milli: i64,
    pub(crate) points: Vec<Option<ScrollPoint>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ScrollPoint {
    pub(crate) position: String,
    pub(crate) text: String,
    pub(crate) shape: String,
}

pub(crate) fn scroll_round_trip_changes(
    before: Option<&ScrollSample>,
    away: Option<&ScrollSample>,
    returned: Option<&ScrollSample>,
    confirmed: Option<&ScrollSample>,
) -> Vec<serde_json::Value> {
    let (Some(before), Some(away), Some(returned), Some(confirmed)) =
        (before, away, returned, confirmed)
    else {
        return Vec::new();
    };
    if before.points.len() != away.points.len()
        || before.points.len() != returned.points.len()
        || before.points.len() != confirmed.points.len()
        || before.offset_milli != returned.offset_milli
        || returned.offset_milli != confirmed.offset_milli
        || away.offset_milli == before.offset_milli
    {
        return Vec::new();
    }
    let mut items = Vec::new();
    for index in 0..before.points.len() {
        let (Some(before), Some(away), Some(returned), Some(confirmed)) = (
            &before.points[index],
            &away.points[index],
            &returned.points[index],
            &confirmed.points[index],
        ) else {
            continue;
        };
        if before.position != returned.position
            || returned.position != confirmed.position
            || before.shape != returned.shape
            || returned.shape != confirmed.shape
            || returned.text != confirmed.text
            || before.text == returned.text
            || before.text == away.text
        {
            continue;
        }
        items.push(serde_json::json!({
            "pos": before.position,
            "before": before.text,
            "after": returned.text,
        }));
    }
    items
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(key: &str, identity: u64) -> NativeNode {
        NativeNode {
            key: key.into(),
            identity,
            role: "button".into(),
        }
    }

    #[test]
    fn focus_loss_requires_retained_target_and_window_focus() {
        let arm = FocusArm {
            key: "id:save".into(),
            identity: 7,
            role: "button".into(),
            dialog_count: 0,
        };
        let clean_after = FocusAfter {
            target: node("id:save", 7),
            focused_identity: Some(7),
            focus_is_window: false,
            dialog_count: 0,
            same_screen: true,
        };
        let lost_after = FocusAfter {
            focused_identity: Some(99),
            focus_is_window: true,
            ..clean_after.clone()
        };
        assert!(focus_was_lost(Some(&arm), Some(&lost_after)));
        assert!(!focus_was_lost(Some(&arm), Some(&clean_after)));
        let dialog = FocusAfter {
            dialog_count: 1,
            ..lost_after.clone()
        };
        assert!(!focus_was_lost(Some(&arm), Some(&dialog)));
        assert!(!focus_was_lost(Some(&arm), None));
        let rebuilt = FocusAfter {
            target: node("id:save", 8),
            ..lost_after
        };
        assert!(!focus_was_lost(Some(&arm), Some(&rebuilt)));
    }

    fn scroll_sample(offset_milli: i64, text: Option<&str>) -> ScrollSample {
        ScrollSample {
            offset_milli,
            points: vec![text.map(|text| ScrollPoint {
                position: "y=40".into(),
                text: text.into(),
                shape: "listitem|100|20".into(),
            })],
        }
    }

    #[test]
    fn scroll_round_trip_requires_exact_restoration_and_confirmation() {
        let before = scroll_sample(0, Some("row a"));
        let away = scroll_sample(100_000, Some("row z"));
        let changed = scroll_sample(0, Some("row b"));
        assert_eq!(
            scroll_round_trip_changes(Some(&before), Some(&away), Some(&changed), Some(&changed),)
                .len(),
            1
        );
        assert!(scroll_round_trip_changes(
            Some(&before),
            Some(&away),
            Some(&before),
            Some(&before),
        )
        .is_empty());
        let drifting = scroll_sample(1, Some("row b"));
        assert!(scroll_round_trip_changes(
            Some(&before),
            Some(&away),
            Some(&drifting),
            Some(&drifting),
        )
        .is_empty());
        assert!(
            scroll_round_trip_changes(Some(&before), None, Some(&changed), Some(&changed))
                .is_empty()
        );
    }
}
