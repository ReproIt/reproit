part of '../reproit_explorer.dart';

// ===========================================================================
// HANG oracle (EXPLORE:HANG) - deterministic watchdog, SIM-ONLY.
//
// The distinct-from-jank freeze signal: an action whose pump/settle never
// reaches a quiescent frame within a FIXED budget. Jank (already wired via the
// frame manifest's jank_pct in fuzz.rs) is "slow frames"; a HANG is "no
// progress at all" - an action that wedges the UI thread (a synchronous busy
// loop, an await that never completes, an animation that never settles). We
// detect it by driving a BOUNDED settle and checking whether the binding still
// reports transient callbacks / scheduled frames pending after the budget
// elapsed: if the app is still trying to produce frames (or a synchronous
// handler blocked so long the budget's worth of real wall-clock passed before a
// single pump returned), the action did not settle and we emit EXPLORE:HANG.
//
// DETERMINISM: keyed by (from, action) like the web HANG oracle and like jank,
// and bucketed into a single coarse floor (HANG_FLOOR_MS) carried as `bucket`,
// so timing jitter cannot flip the verdict's IDENTITY - the finding id is the
// (from, action) pair, already deterministic for a fixed seed. The wall-clock
// read only gates WHETHER to emit; the marker content is discrete.
//
// SIM-ONLY: the headless (flutter test) binding uses a FAKE async clock, so a
// real wall-clock watchdog reads zero elapsed and `hasScheduledFrame` reflects
// the fake pump, not a real freeze. So this oracle lives on the simulator
// explorer only (parity with JANK, which is also sim-only). See the headless
// file's ORACLE SCOPE banner.
const int hangFloorMs = 2000;
const int hangPumpStepMs = 100;

/// Drive a bounded settle for [budgetMs] and report whether the action HUNG:
/// true iff, after pumping the whole budget in fixed steps, the binding still
/// has a frame scheduled (the app never reached quiescence) OR the real elapsed
/// wall-clock exceeded the budget by the hang floor (a synchronous handler
/// blocked the thread past the freeze floor). Returns the bucket to emit, or
/// null when the action settled cleanly within budget.
///
/// This REPLACES the plain settle() for the action it guards: it pumps the same
/// total budget, so the walk's timing is unchanged; it only ADDS the verdict.
Future<int?> settleWatchdog(WidgetTester t, int budgetMs) async {
  final sw = Stopwatch()..start();
  final steps = budgetMs ~/ hangPumpStepMs;
  for (var i = 0; i < steps; i++) {
    try {
      await t.pump(const Duration(milliseconds: hangPumpStepMs));
    } catch (_) {
      // A pump that throws (e.g. a handler error) is drained by the caller; do
      // not treat it as a hang on its own.
    }
  }
  final elapsedMs = sw.elapsedMilliseconds;
  // Signal 1: real wall-clock blew far past the budget -> a synchronous handler
  // froze the UI thread (the budget's worth of pumps took >> budget to return).
  final blocked = elapsedMs - budgetMs;
  // Signal 2: after the full settle budget the framework STILL wants to draw a
  // frame -> the screen never reached a quiescent state (an unsettling animation
  // / a never-completing relayout), which is a freeze for an action that should
  // have settled.
  final stillScheduling = t.binding.hasScheduledFrame;
  if (blocked >= hangFloorMs || stillScheduling) {
    return hangFloorMs;
  }
  return null;
}
