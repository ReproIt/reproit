//! Authored, bounded partial-order reduction for multi-actor schedules.

use super::IndependentActionPair;
use anyhow::{bail, Result};
use std::collections::{BTreeSet, HashSet};

const MAX_INDEPENDENT_PAIRS: usize = 128;
const MAX_SCHEDULE_STEPS: usize = 1_024;
const MAX_ACTION_BYTES: usize = 512;

pub(super) fn validate_independence(pairs: &[IndependentActionPair]) -> Result<()> {
    if pairs.len() > MAX_INDEPENDENT_PAIRS {
        bail!("independentActions exceeds the {MAX_INDEPENDENT_PAIRS} pair limit");
    }
    let mut unique = BTreeSet::new();
    for pair in pairs {
        if pair.left.is_empty()
            || pair.right.is_empty()
            || pair.left.len() > MAX_ACTION_BYTES
            || pair.right.len() > MAX_ACTION_BYTES
            || pair.left == pair.right
        {
            bail!("independentActions must contain two distinct bounded actions");
        }
        if !unique.insert(ordered_pair(&pair.left, &pair.right)) {
            bail!("independentActions contains a duplicate pair");
        }
    }
    Ok(())
}

pub(super) fn canonicalize(
    schedule: &[(String, String)],
    pairs: &[IndependentActionPair],
) -> Result<Vec<(String, String)>> {
    validate_independence(pairs)?;
    if pairs.is_empty() {
        return Ok(schedule.to_vec());
    }
    if schedule.len() > MAX_SCHEDULE_STEPS {
        bail!("multi-actor schedule exceeds the {MAX_SCHEDULE_STEPS} step reduction limit");
    }
    let independent = pairs
        .iter()
        .map(|pair| ordered_pair(&pair.left, &pair.right))
        .collect::<HashSet<_>>();
    let mut outgoing = vec![Vec::new(); schedule.len()];
    let mut indegree = vec![0_usize; schedule.len()];
    for left in 0..schedule.len() {
        for right in left + 1..schedule.len() {
            if dependent(&schedule[left], &schedule[right], &independent) {
                outgoing[left].push(right);
                indegree[right] += 1;
            }
        }
    }
    let mut ready = BTreeSet::new();
    for (index, step) in schedule.iter().enumerate() {
        if indegree[index] == 0 {
            ready.insert((step.0.clone(), step.1.clone(), index));
        }
    }
    let mut canonical = Vec::with_capacity(schedule.len());
    while let Some(next) = ready.pop_first() {
        let index = next.2;
        canonical.push(schedule[index].clone());
        for &dependent_index in &outgoing[index] {
            indegree[dependent_index] -= 1;
            if indegree[dependent_index] == 0 {
                let step = &schedule[dependent_index];
                ready.insert((step.0.clone(), step.1.clone(), dependent_index));
            }
        }
    }
    debug_assert_eq!(canonical.len(), schedule.len());
    Ok(canonical)
}

fn dependent(
    left: &(String, String),
    right: &(String, String),
    independent: &HashSet<(String, String)>,
) -> bool {
    left.0 == right.0 || !independent.contains(&ordered_pair(&left.1, &right.1))
}

fn ordered_pair(left: &str, right: &str) -> (String, String) {
    if left <= right {
        (left.to_string(), right.to_string())
    } else {
        (right.to_string(), left.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair(left: &str, right: &str) -> IndependentActionPair {
        IndependentActionPair {
            left: left.into(),
            right: right.into(),
        }
    }

    #[test]
    fn authored_independence_canonicalizes_equivalent_schedules() {
        let declarations = vec![pair("tap:key:a", "tap:key:b")];
        let first = vec![
            ("bob".into(), "tap:key:b".into()),
            ("alice".into(), "tap:key:a".into()),
        ];
        let second = vec![
            ("alice".into(), "tap:key:a".into()),
            ("bob".into(), "tap:key:b".into()),
        ];
        assert_eq!(
            canonicalize(&first, &declarations).unwrap(),
            canonicalize(&second, &declarations).unwrap()
        );
    }

    #[test]
    fn unknown_dependence_preserves_schedule_order() {
        let schedule = vec![
            ("bob".into(), "tap:key:b".into()),
            ("alice".into(), "tap:key:a".into()),
        ];
        assert_eq!(canonicalize(&schedule, &[]).unwrap(), schedule);
    }

    #[test]
    fn same_actor_actions_are_never_reordered() {
        let schedule = vec![
            ("alice".into(), "tap:key:b".into()),
            ("alice".into(), "tap:key:a".into()),
        ];
        assert_eq!(
            canonicalize(&schedule, &[pair("tap:key:a", "tap:key:b")]).unwrap(),
            schedule
        );
    }
}
