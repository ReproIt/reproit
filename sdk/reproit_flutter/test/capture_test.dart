import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:reproit_flutter/reproit_flutter.dart';

// Proves the SDK captures a real state graph from a live widget tree: it reads
// the semantics tree, computes signatures, and emits a tap-labeled edge when the
// visible labels change.
void main() {
  tearDown(ReproIt.dispose); // idempotent safety net

  testWidgets('emits initial state + a tap-labeled edge on a state change',
      (tester) async {
    final events = <Map<String, dynamic>>[];
    ReproIt.init(ReproItConfig(
      appId: 'test',
      onEvent: events.add,
      debounce: const Duration(milliseconds: 50),
    ));

    await tester.pumpWidget(const _TwoScreenApp());
    await tester.pump(const Duration(milliseconds: 80)); // settle -> initial snapshot

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
    expect(tapEdge['action'], 'tap:Go B');
    expect(tapEdge['to'], isNot(sigA));
    final toLabels = (tapEdge['labels'] as List).cast<String>();
    expect(toLabels, containsAll(<String>['Go A', 'Screen B']));
    // The signature is STRUCTURAL (canonical descriptor of the node tree), not a
    // hash of labels: it is a valid 8-hex sig and distinct from the prior state.
    expect(tapEdge['to'], matches(RegExp(r'^[0-9a-f]{8}$')));
    expect(tapEdge['to'], isNot(sigA));
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
