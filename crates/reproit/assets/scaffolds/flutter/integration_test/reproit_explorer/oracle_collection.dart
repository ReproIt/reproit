part of '../reproit_explorer.dart';

String observeScenarioState(
  WidgetTester tester,
  ExplorerRuntime runtime,
  Set<String> seen,
) {
  final snap = snapshot(tester);
  emitJson('FUZZ:OBS', {
    "sig": snap.sig,
    if (snap.anchor != null) "route": snap.anchor,
    "labels": snap.labels.take(maxLabelsPerState).toList(),
    "elements": roleElements(snap),
  });
  if (seen.add(snap.sig)) {
    emitJson('EXPLORE:STATE', {
      "sig": snap.sig,
      if (snap.anchor != null) "route": snap.anchor,
      "labels": snap.labels.take(maxLabelsPerState).toList(),
      "elements": stateElements(snap),
    });
    runtime.emit(
      'EXPLORE:GROUNDTRUTH ${jsonEncode(groundTruth(tester, snap.sig))}',
    );
  }
  return snap.sig;
}
