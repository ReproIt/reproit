//! Graph topology helpers used by permission and custom invariants.

use crate::model::map::RunObs;

/// States that are dead ends in this run's observed graph: a state that was
/// observed AND has at least one outgoing edge recorded OR is the start, but
/// whose ONLY outgoing edges are `back`. A state with no outgoing edge at all
/// is a dead end iff it is not the start (the start with no edges just means an
/// empty walk). We treat "no non-back exit" as the dead-end condition, which is
/// exactly PLANTED-BUG 6 (the Advanced screen: reachable, but its only exit is
/// system back).
pub(super) fn permission_traps(obs: &RunObs) -> Vec<String> {
    // Routes (URL path / framework anchor) that have a forward exit from SOME
    // state on them. On a dynamic single-page site, one logical page churns into
    // several structural snapshots (animation, lazy render) that share a route;
    // the snapshot where the walk's budget ran out has no recorded exit and would
    // look like a sink. If a same-route sibling does have a forward exit, it is
    // the same page and the walk could leave it, so the exit-less snapshot is an
    // artifact, not a dead end. A genuinely trapped screen has its own route and
    // is unaffected. Empty when no runner reports routes (TUI/desktop), so the
    // predicate is unchanged there.
    // route -> the label sets of states with a forward exit, from THIS seed's
    // edges plus the aggregate-map fold-in. A sink on such a route is excused only
    // when its labels are a SUBSET of one of these escapable siblings -- a
    // same-or-reduced render of that page (animation churn). A structurally
    // DISTINCT screen sharing the URL (a section toggle with no route change)
    // shows labels the escapable page lacks, so it is not a subset and stays
    // flagged. Empty when no runner reports routes (TUI/desktop), so the predicate
    // is unchanged there.
    let mut route_exit_labels: std::collections::BTreeMap<
        String,
        Vec<std::collections::BTreeSet<String>>,
    > = std::collections::BTreeMap::new();
    for (from, action, to) in &obs.edges {
        if action != "back" && to != from {
            if let Some(route) = obs.routes.get(from) {
                let labels: std::collections::BTreeSet<String> =
                    label_set(obs, from).into_iter().collect();
                route_exit_labels
                    .entry(route.clone())
                    .or_default()
                    .push(labels);
            }
        }
    }
    for (route, sets) in &obs.escapable_route_labels {
        route_exit_labels
            .entry(route.clone())
            .or_default()
            .extend(sets.iter().cloned());
    }
    // route -> label sets of states that offered tappables on SOME snapshot. A
    // zero-tappable snapshot of the same route (header nav scrolled offscreen, a
    // partial render) is not a proven sink IF it is a same-or-reduced render of a
    // tappable-bearing sibling (its labels are a subset). A distinct content-only
    // screen sharing the URL (an "Advanced" pane with no controls) shows labels no
    // tappable sibling has, so it is not excused here.
    let mut routes_with_tappables: std::collections::BTreeMap<
        String,
        Vec<std::collections::BTreeSet<String>>,
    > = std::collections::BTreeMap::new();
    for (sig, &n) in &obs.tappables {
        if n > 0 {
            if let Some(route) = obs.routes.get(sig) {
                let labels: std::collections::BTreeSet<String> =
                    label_set(obs, sig).into_iter().collect();
                routes_with_tappables
                    .entry(route.clone())
                    .or_default()
                    .push(labels);
            }
        }
    }

    let mut out = Vec::new();
    for sig in obs.states.keys() {
        let is_start = obs.start.as_deref() == Some(sig.as_str());
        // Reachable as a destination of some edge, or the start state.
        let reachable = is_start || obs.edges.iter().any(|(_, _, to)| to == sig);
        if !reachable {
            continue;
        }
        // A START state the walk never acted from is an empty/unproductive walk,
        // not a proven sink (this fn's contract, and the common shape of a web
        // seed that churned without recording an exit). Only the start gets this
        // pass: a NON-start state reached with no exit IS a genuine sink (the
        // Advanced-screen planted bug), so it stays flagged.
        let acted_from = obs.edges.iter().any(|(from, _, _)| from == sig);
        if is_start && !acted_from {
            continue;
        }
        let has_forward_exit = obs
            .edges
            .iter()
            .any(|(from, action, to)| from == sig && action != "back" && to != sig);
        if has_forward_exit {
            continue;
        }
        // Same page has a forward exit -> this is a transient snapshot of an
        // escapable page, not a real sink. Two sources: a same-route sibling in
        // THIS seed's sparse graph, and the AGGREGATE map's escapable routes
        // folded in by the caller (covers the common case where one seed visited
        // the page only as its budget terminus).
        if let Some(route) = obs.routes.get(sig) {
            if let Some(sibling_sets) = route_exit_labels.get(route) {
                let sink_labels: std::collections::BTreeSet<String> =
                    label_set(obs, sig).into_iter().collect();
                // Suppress only a same-or-reduced render of an escapable sibling:
                // the sink shows nothing the escapable page does not already show.
                // A distinct screen at the same URL carries labels no escapable
                // sibling has, so it is not a subset and is correctly flagged.
                if sibling_sets.iter().any(|s| sink_labels.is_subset(s)) {
                    continue;
                }
            }
        }
        // Unexplored terminus, not a proven sink: the state OFFERED tappable
        // elements the walk never tapped (more tappables than recorded tap
        // actions from it). A real dead end either offers no forward action or
        // has all its actions exhausted with no exit; a leaf page reached as the
        // budget terminus (e.g. a blog article whose header nav was deduped after
        // being tried elsewhere) still has untapped nav and is not a trap.
        // tappables=0 (no element data, as in unit fixtures) never triggers this,
        // so a genuine no-action sink stays flagged.
        let offered = obs.tappables.get(sig).copied().unwrap_or(0);
        if offered > 0 {
            // Count any FORWARD action tried from this state, not just `tap:`. The
            // forward-action verb differs by platform -- web/native a11y tap and
            // type, the TUI presses keys (`key:Down`/`key:Enter`) -- so keying off
            // `tap:` alone made every TUI state look like all its offered elements
            // were untried (suppression always fired, real TUI sinks never flagged).
            // `!= "back"` is the platform-neutral "the walk tried something here".
            let tried = obs
                .edges
                .iter()
                .filter(|(from, action, _)| from == sig && *action != "back")
                .count();
            if offered > tried {
                continue;
            }
        } else if let Some(route) = obs.routes.get(sig) {
            // This snapshot saw zero tappables. If a same-route sibling that DID
            // offer tappables is a superset of this one's labels, this is a
            // transient/partial render of that page, not a sink. A distinct
            // content-only screen at the same URL has labels no tappable sibling
            // carries, so it stays flagged.
            if let Some(sibling_sets) = routes_with_tappables.get(route) {
                let sink_labels: std::collections::BTreeSet<String> =
                    label_set(obs, sig).into_iter().collect();
                if sibling_sets.iter().any(|s| sink_labels.is_subset(s)) {
                    continue;
                }
            }
        }
        out.push(sig.clone());
    }
    out
}

pub(super) fn label_set(obs: &RunObs, sig: &str) -> Vec<String> {
    obs.states.get(sig).cloned().unwrap_or_default()
}

pub(super) fn screen_hint(labels: &[String]) -> String {
    if labels.is_empty() {
        String::new()
    } else {
        format!(
            " [{}]",
            labels
                .iter()
                .take(3)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}
