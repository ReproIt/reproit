import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:reproit_flutter/reproit_flutter.dart';

// Proves the SDK captures a real state graph from a live widget tree: it reads
// the semantics tree, computes signatures, and emits a structural tap edge plus
// display label when the visible labels change.
void main() {
  test('state preservation proves only an explicit authoritative boundary', () {
    var state = 'draft:present';
    ReproIt.preserveState(
        'draft',
        ReproItStatePreservationContract(
            boundaries: const {ReproItStateBoundary.rotation},
            sample: () => ReproItStructuralObservation(
                key: 'checkout',
                state: state,
                authoritative: true,
                settled: true)));
    expect(
        ReproIt.stateBoundary(
                ReproItStateBoundary.rotation, ReproItBoundaryPhase.before)[0]
            .status,
        ReproItContractStatus.valid);
    state = 'draft:empty';
    final result = ReproIt.stateBoundary(
        ReproItStateBoundary.rotation, ReproItBoundaryPhase.after)[0];
    expect(result.status, ReproItContractStatus.proven);
    expect(result.id, 'state-preservation:rotation:draft');
    ReproIt.debugClearStructuralContracts();
  });

  test('action effects evaluate route and local state structurally', () {
    var observation = const ReproItActionEffectObservation(
        route: 'cart', state: 'idle', authoritative: true, settled: true);
    ReproIt.actionEffect(
        'checkout',
        ReproItActionEffectContract(
            sample: () => observation,
            route: const ReproItTargetEffect('receipt'),
            state: const ReproItChangeEffect(target: 'complete')));
    ReproIt.actionBegin('checkout');
    observation = const ReproItActionEffectObservation(
        route: 'cart', state: 'complete', authoritative: true, settled: true);
    expect(
        ReproIt.actionEnd('checkout')
            .where((r) => r.status == ReproItContractStatus.proven)
            .map((r) => r.id),
        [
          'action-effect:checkout:route',
        ]);
    ReproIt.debugClearStructuralContracts();
  });

  test('process recreation requires persistent baseline callbacks', () {
    var state = 'present';
    ReproItStructuralObservation? saved;
    ReproIt.preserveState(
        'draft',
        ReproItStatePreservationContract(
            boundaries: const {ReproItStateBoundary.processRecreation},
            sample: () => ReproItStructuralObservation(
                key: 'checkout',
                state: state,
                authoritative: true,
                settled: true),
            saveBaseline: (_, value) {
              saved = value;
              return true;
            },
            loadBaseline: (_) => saved));
    expect(
        ReproIt.stateBoundary(ReproItStateBoundary.processRecreation,
                ReproItBoundaryPhase.before)[0]
            .status,
        ReproItContractStatus.valid);
    state = 'empty';
    expect(
        ReproIt.stateBoundary(ReproItStateBoundary.processRecreation,
                ReproItBoundaryPhase.after)[0]
            .status,
        ReproItContractStatus.proven);
    ReproIt.debugClearStructuralContracts();
  });

  test('focused input requires reveal and two stable hidden samples', () {
    var reveals = 0;
    ReproIt.focusedInput('email',
        reveal: () {
          reveals++;
          return true;
        },
        sample: () => const ReproItFocusObservation(
            key: 'key:email',
            focusedEditable: true,
            field: Rect.fromLTWH(0, 700, 100, 40),
            usableViewport: Rect.fromLTWH(0, 0, 390, 500),
            exactKeyboardRect: true));
    expect(ReproIt.debugFocusMarker(), isNull);
    expect(reveals, 1);
    expect(ReproIt.debugFocusMarker(), isNull);
    expect(ReproIt.debugFocusMarker(),
        contains('focused-input-obscured:key:email'));
    ReproIt.debugClearFocusedInputs();
  });
  test('indicator relationship needs two stable global samples', () {
    ReproIt.indicator('liked',
        dependentKey: 'key:badge',
        ownerKey: 'key:liked',
        containerKey: 'key:tabs',
        sample: () => const ReproItIndicatorGeometry(
            indicator: Rect.fromLTWH(180, 800, 10, 10),
            owner: Rect.fromLTWH(160, 700, 60, 50),
            container: Rect.fromLTWH(0, 680, 390, 100)));
    expect(ReproIt.debugIndicatorMarker(), isNull);
    expect(ReproIt.debugIndicatorMarker(), contains('escaped-container'));
    ReproIt.debugClearIndicators();
  });
  tearDown(ReproIt.dispose); // idempotent safety net

  testWidgets('emits initial state + structural tap edge on a state change',
      (tester) async {
    final events = <Map<String, dynamic>>[];
    ReproIt.init(ReproItConfig(
      appId: 'test',
      onEvent: events.add,
      debounce: const Duration(milliseconds: 50),
    ));

    await tester.pumpWidget(const _TwoScreenApp());
    await tester
        .pump(const Duration(milliseconds: 80)); // settle -> initial snapshot

    // Navigate: the visible labels change -> a new state.
    await tester.tap(find.text('Go B'));
    await tester.pump(); // process the tap + rebuild
    await tester.pump(const Duration(milliseconds: 80)); // settle -> snapshot

    // Dispose now (cancels timers + the SemanticsHandle) so assertions below
    // can't leak resources even if one fails.
    ReproIt.dispose();

    final edges = events.where((e) => e['kind'] == 'edge').toList();
    expect(edges, isNotEmpty, reason: 'should capture the initial state');
    final initial = edges.first;
    expect(initial['action'], 'load');
    final sigA = initial['to'] as String;
    expect(sigA, matches(RegExp(r'^[0-9a-f]{8}$')));
    expect((initial['labels'] as List).contains('Go B'), isTrue);

    final tapEdge = edges.last;
    expect(tapEdge['from'], sigA);
    expect(tapEdge['action'], 'tap:key:goB');
    expect(tapEdge['label'], 'Go B');
    expect(tapEdge['to'], isNot(sigA));
    final toLabels = (tapEdge['labels'] as List).cast<String>();
    expect(toLabels, containsAll(<String>['Go A', 'Screen B']));
    // The signature is STRUCTURAL (canonical descriptor of the node tree), not a
    // hash of labels: it is a valid 8-hex sig and distinct from the prior state.
    expect(tapEdge['to'], matches(RegExp(r'^[0-9a-f]{8}$')));
    expect(tapEdge['to'], isNot(sigA));
  });

  testWidgets('captureBug emits an exact structural tester capture',
      (tester) async {
    final events = <Map<String, dynamic>>[];
    ReproIt.init(ReproItConfig(appId: 'test', onEvent: events.add));
    await tester.pumpWidget(const _TwoScreenApp());
    await tester.pump(const Duration(milliseconds: 400));

    expect(ReproIt.captureBug(), isTrue);
    final event = events.lastWhere((e) => e['oracle'] == 'tester-capture');
    expect(event['sig'], isNotEmpty);
    expect(event['findingIdentity']['boundary'], event['sig']);
    expect(event['findingIdentity']['invariant'], 'tester-observed-failure');
    ReproIt.dispose();
  });

  testWidgets('production contract capture keeps the exact invariant identity',
      (tester) async {
    final events = <Map<String, dynamic>>[];
    ReproIt.init(ReproItConfig(appId: 'test', onEvent: events.add));
    await tester.pumpWidget(const _TwoScreenApp());
    await tester.pump(const Duration(milliseconds: 400));
    var state = 'present';
    ReproIt.preserveState(
        'draft',
        ReproItStatePreservationContract(
            boundaries: const {ReproItStateBoundary.rotation},
            sample: () => ReproItStructuralObservation(
                key: 'checkout',
                state: state,
                authoritative: true,
                settled: true)));
    ReproIt.stateBoundary(
        ReproItStateBoundary.rotation, ReproItBoundaryPhase.before);
    state = 'empty';
    ReproIt.stateBoundary(
        ReproItStateBoundary.rotation, ReproItBoundaryPhase.after);
    final event = events.lastWhere((e) => e['oracle'] == 'invariant');
    expect(event['findingIdentity']['invariant'],
        'state-preservation:rotation:draft');
    expect(event['findingIdentity']['boundary'], event['sig']);
    ReproIt.debugClearStructuralContracts();
    ReproIt.dispose();
  });
}

class _TwoScreenApp extends StatefulWidget {
  const _TwoScreenApp();
  @override
  State<_TwoScreenApp> createState() => _TwoScreenAppState();
}

class _TwoScreenAppState extends State<_TwoScreenApp> {
  bool _b = false;
  @override
  Widget build(BuildContext context) {
    // The two states are STRUCTURALLY distinct (screen B adds a second button),
    // so the structural signature changes and an edge is emitted. (Two screens
    // that differ only in text would, correctly, hash identically now.)
    return MaterialApp(
      theme: ThemeData(useMaterial3: false),
      home: Scaffold(
        body: Center(
          child: _b
              ? Column(mainAxisSize: MainAxisSize.min, children: [
                  const Text('Screen B'),
                  ElevatedButton(
                      key: const ValueKey('goA'),
                      onPressed: () => setState(() => _b = false),
                      child: const Text('Go A')),
                  ElevatedButton(
                      key: const ValueKey('extra'),
                      onPressed: () {},
                      child: const Text('Extra')),
                ])
              : Column(mainAxisSize: MainAxisSize.min, children: [
                  const Text('Screen A'),
                  ElevatedButton(
                      key: const ValueKey('goB'),
                      onPressed: () => setState(() => _b = true),
                      child: const Text('Go B')),
                ]),
        ),
      ),
    );
  }
}
