import 'dart:typed_data';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import 'reproit_explorer.dart';

Uint8List pixels(List<int> red) {
  final bytes = <int>[];
  for (final value in red) {
    bytes.addAll([value, 0, 0, 255]);
  }
  return Uint8List.fromList(bytes);
}

class FlickerFixture extends StatefulWidget {
  const FlickerFixture({super.key, required this.transientFlash});

  final bool transientFlash;

  @override
  State<FlickerFixture> createState() => _FlickerFixtureState();
}

class _FlickerFixtureState extends State<FlickerFixture> {
  Color color = Colors.black;

  void transition() {
    if (!widget.transientFlash) {
      setState(() => color = Colors.black);
      return;
    }
    setState(() => color = Colors.white);
    Future<void>.delayed(const Duration(milliseconds: 200), () {
      if (mounted) setState(() => color = Colors.black);
    });
  }

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      home: GestureDetector(
        key: const ValueKey<String>('surface'),
        behavior: HitTestBehavior.opaque,
        onTap: transition,
        child: ColoredBox(color: color),
      ),
    );
  }
}

void main() {
  test('pixel flicker fires only on an intermediate overshoot', () {
    final end = pixels(List<int>.filled(10, 0));
    final start = pixels([255, 255, 0, 0, 0, 0, 0, 0, 0, 0]);
    final flash = pixels(List<int>.filled(10, 255));
    expect(classifyTransitionFlicker([start, flash, end]), {
      'peak': 1.0,
      'frames': 3,
    });
    expect(classifyTransitionFlicker([start, end]), isNull);

    final legitimateStart = pixels(List<int>.filled(10, 255));
    final midpoint = pixels([255, 255, 255, 255, 255, 0, 0, 0, 0, 0]);
    expect(classifyTransitionFlicker([legitimateStart, midpoint, end]), isNull);
  });

  test('jank requires a severe frame or a sustained pair', () {
    expect(classifyTransitionJank(const <List<int>>[]), isNull);
    expect(
      classifyTransitionJank(const [
        <int>[0, 60000, 30000],
      ]),
      isNull,
    );
    expect(
      classifyTransitionJank(const [
        <int>[0, 360000, 0],
      ]),
      {'bucket': 100, 'count': 1},
    );
    expect(
      classifyTransitionJank(const [
        <int>[0, 60000, 50000],
        <int>[1, 70000, 40000],
      ]),
      {'bucket': 100, 'count': 2},
    );
  });

  test('only Flutter explicit RenderFlex diagnostics classify as overflow', () {
    expect(
      classifyFrameworkOverflow(
        'A RenderFlex overflowed by 12.4 pixels on the right.',
      ),
      {'key': 'render-flex', 'edge': 'right', 'by': 13},
    );
    expect(
      classifyFrameworkOverflow('BoxConstraints forces an infinite width.'),
      isNull,
    );
  });

  testWidgets('real RenderFlex overflow supplies the framework authority', (
    tester,
  ) async {
    await tester.pumpWidget(
      const Directionality(
        textDirection: TextDirection.ltr,
        child: Center(
          child: SizedBox(
            width: 10,
            child: Row(children: [SizedBox(width: 100, height: 10)]),
          ),
        ),
      ),
    );
    final reported = tester.takeException();
    expect(reported, isNotNull);
    expect(classifyFrameworkOverflow(reported.toString()), isNotNull);
  });

  testWidgets('presented Flutter frames expose a transient flash', (
    tester,
  ) async {
    const runtime = SimulatorExplorerRuntime();
    await tester.pumpWidget(const FlickerFixture(transientFlash: true));
    await tester.pump();
    await runtime.beginTransitionFrames(tester);
    await tester.tap(find.byKey(const ValueKey<String>('surface')));
    await runtime.settleAfterTap(tester, (target, milliseconds) async {}, 500);
    expect(runtime.finishTransitionFlicker(), isNotNull);
  });

  testWidgets('direct Flutter transition is a negative control', (
    tester,
  ) async {
    const runtime = SimulatorExplorerRuntime();
    await tester.pumpWidget(const FlickerFixture(transientFlash: false));
    await tester.pump();
    await runtime.beginTransitionFrames(tester);
    await tester.tap(find.byKey(const ValueKey<String>('surface')));
    await runtime.settleAfterTap(tester, (target, milliseconds) async {}, 500);
    expect(runtime.finishTransitionFlicker(), isNull);
  });
}
