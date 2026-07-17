// Dogfood for the LIFECYCLE-metamorphic oracles ROTATION + BACKGROUND-RESTORE
// (Flutter scaffold :: rotationCheck /
// backgroundCheck). The transform and comparison bodies below are parity copies
// because the scaffold is not part of the published SDK. If the scaffold logic
// changes, change these fixtures too. This validates both directions:
//   1. the rotation transform (view.physicalSize swap + round-trip back) reaches the
//      app and is self-restoring
//   2. an app that permanently loses content on the orientation change  -> fires
//   3. an app whose content survives the rotation round-trip            -> silent
//   4. the background transform (paused -> resumed) reaches the app
//   5. an app that lands on a different screen after restore            -> fires
//   6. an app that returns to the same screen after restore             -> silent
import 'dart:ui' show Size;

import 'package:flutter/material.dart';
import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';

// PARITY COPY of the STRUCTURAL signal the oracles compare: the scaffold folds a
// structural-only signature (roles + tree shape, value-state excluded). Here we
// stand in for it with the sorted set of visible Text content -- a deterministic
// structural fingerprint that changes exactly when content is lost or the screen
// is replaced, which is what the metamorphic relation asserts about.
String structuralSnapshot(WidgetTester t) {
  final texts = <String>[];
  for (final e in find.byType(Text).evaluate()) {
    final w = e.widget as Text;
    final d = w.data;
    if (d != null && d.isNotEmpty) texts.add(d);
  }
  texts.sort();
  return texts.join('|');
}

// PARITY COPY of Flutter scaffold::rotationCheck (the transform half):
// swap the surface width/height (portrait <-> landscape), reflow, then rotate
// BACK to the original orientation. Round-trip identity at the original
// orientation is what makes it false-positive-free.
Future<void> rotate(WidgetTester t) async {
  final view = t.view;
  final origPhys = view.physicalSize;
  view.physicalSize = Size(origPhys.height, origPhys.width);
  await t.pumpAndSettle();
  view.physicalSize = origPhys;
  await t.pumpAndSettle();
}

// PARITY COPY of Flutter scaffold::backgroundCheck (the transform half):
// drive the app lifecycle to the background (inactive -> paused) then restore it
// (inactive -> resumed).
Future<void> backgroundRestore(WidgetTester t) async {
  t.binding.handleAppLifecycleStateChanged(AppLifecycleState.inactive);
  t.binding.handleAppLifecycleStateChanged(AppLifecycleState.paused);
  await t.pumpAndSettle();
  t.binding.handleAppLifecycleStateChanged(AppLifecycleState.inactive);
  t.binding.handleAppLifecycleStateChanged(AppLifecycleState.resumed);
  await t.pumpAndSettle();
}

/// A screen whose three items survive a rotation (the correct app): static
/// content that reflows but is never dropped.
class RotationStable extends StatelessWidget {
  const RotationStable({super.key});
  @override
  Widget build(BuildContext context) => const Column(
        children: [Text('Home'), Text('Profile'), Text('Settings')],
      );
}

/// A screen that mishandles the orientation change: on the first metric change to
/// portrait it PERMANENTLY clears its content and never rebuilds it, so rotating
/// back does not bring the screen back -- state lost across the rotation
/// lifecycle. The violation the oracle must catch.
class RotationLosing extends StatefulWidget {
  const RotationLosing({super.key});
  @override
  State<RotationLosing> createState() => _RotationLosingState();
}

class _RotationLosingState extends State<RotationLosing> {
  bool _broke = false;

  @override
  Widget build(BuildContext context) {
    final size = MediaQuery.of(context).size;
    // First time the screen goes portrait, latch the break PERMANENTLY (a
    // post-frame setState, so the rebuild is clean). Rotating back never clears
    // it, so the content is gone for good -- the lifecycle bug the oracle catches.
    if (size.height > size.width && !_broke) {
      WidgetsBinding.instance.addPostFrameCallback((_) {
        if (mounted && !_broke) setState(() => _broke = true);
      });
    }
    return _broke
        ? const SizedBox.shrink()
        : const Column(
            children: [Text('Home'), Text('Profile'), Text('Settings')],
          );
  }
}

/// A screen that returns to itself after backgrounding (the correct app): it
/// ignores the lifecycle and keeps its content.
class BackgroundStable extends StatelessWidget {
  const BackgroundStable({super.key});
  @override
  Widget build(BuildContext context) => const Column(
        children: [Text('Home'), Text('Profile'), Text('Settings')],
      );
}

/// A screen that mishandles the background lifecycle: on `paused` it locks and
/// NEVER unlocks on `resumed`, so restoring the app drops the user on a different
/// screen. The violation the oracle must catch.
class BackgroundLosing extends StatefulWidget {
  const BackgroundLosing({super.key});
  @override
  State<BackgroundLosing> createState() => _BackgroundLosingState();
}

class _BackgroundLosingState extends State<BackgroundLosing>
    with WidgetsBindingObserver {
  bool _locked = false;
  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);
  }

  @override
  void dispose() {
    WidgetsBinding.instance.removeObserver(this);
    super.dispose();
  }

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    if (state == AppLifecycleState.paused && !_locked) {
      setState(() => _locked = true);
    }
  }

  @override
  Widget build(BuildContext context) => _locked
      ? const Text('Locked')
      : const Column(
          children: [Text('Home'), Text('Profile'), Text('Settings')],
        );
}

Widget fixture(Widget child) => MaterialApp(home: Scaffold(body: child));

void main() {
  testWidgets('rotation-losing screen fires (structure not restored)',
      (t) async {
    // Release the surface override so it never leaks between tests.
    addTearDown(t.view.resetPhysicalSize);
    await t.pumpWidget(fixture(const RotationLosing()));
    await t.pumpAndSettle();
    final expected = structuralSnapshot(t);
    expect(expected, 'Home|Profile|Settings');
    await rotate(t);
    final got = structuralSnapshot(t);
    expect(got, isNot(expected),
        reason: 'content lost on rotation never came back on the round-trip');
  });

  testWidgets('rotation-stable screen stays silent (round-trip identity)',
      (t) async {
    addTearDown(t.view.resetPhysicalSize);
    await t.pumpWidget(fixture(const RotationStable()));
    await t.pumpAndSettle();
    final expected = structuralSnapshot(t);
    await rotate(t);
    expect(structuralSnapshot(t), expected,
        reason: 'a reflow that restores must not fire the oracle');
  });

  testWidgets('background-losing screen fires (lands elsewhere on restore)',
      (t) async {
    await t.pumpWidget(fixture(const BackgroundLosing()));
    await t.pumpAndSettle();
    final expected = structuralSnapshot(t);
    expect(expected, 'Home|Profile|Settings');
    await backgroundRestore(t);
    final got = structuralSnapshot(t);
    expect(got, isNot(expected),
        reason: 'restore dropped the user on the lock screen, not Home');
    expect(got, 'Locked');
  });

  testWidgets('background-stable screen stays silent (same screen on restore)',
      (t) async {
    await t.pumpWidget(fixture(const BackgroundStable()));
    await t.pumpAndSettle();
    final expected = structuralSnapshot(t);
    await backgroundRestore(t);
    expect(structuralSnapshot(t), expected,
        reason: 'an app that returns to the same screen must not fire');
  });
}
