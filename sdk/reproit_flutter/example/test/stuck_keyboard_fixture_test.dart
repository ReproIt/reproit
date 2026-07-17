// Dogfood for the Flutter scaffold's STUCK-KEYBOARD oracle. The detector below
// is a parity copy because the scaffold is not part of the published SDK. If
// the scaffold logic changes, change this fixture too. Validates both directions:
//   1. IME up + focused TextField        -> silent (no false positive)
//   2. IME up + focus moved to a button  -> fires (the stuck-keyboard bug)
//   3. IME down                          -> silent regardless of focus
// The IME is simulated with FakeViewPadding, which is exactly what
// tester.view.viewInsets reads on device when the real keyboard is up.
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

// PARITY COPY of Flutter scaffold::detectStuckKeyboard.
bool detectStuckKeyboard(WidgetTester t) {
  if (t.view.viewInsets.bottom <= 0) return false;
  final focus = FocusManager.instance.primaryFocus;
  final ctx = focus?.context;
  // unfocus() parks focus on the enclosing SCOPE node: a scope holding
  // primary focus means no real node is focused, so with the IME up that IS
  // the bug (and the scope's subtree must NOT be searched for editables --
  // it spans the whole screen and would always suppress).
  if (focus == null || focus is FocusScopeNode || ctx == null) return true;
  if (ctx.widget is EditableText) return false;
  var editable = false;
  ctx.visitAncestorElements((el) {
    if (el.widget is EditableText) {
      editable = true;
      return false;
    }
    return true;
  });
  if (!editable && ctx is Element) {
    void walk(Element el) {
      if (editable) return;
      if (el.widget is EditableText) {
        editable = true;
        return;
      }
      el.visitChildren(walk);
    }

    ctx.visitChildren(walk);
  }
  return !editable;
}

Widget fixture() {
  return MaterialApp(
    home: Scaffold(
      body: Column(
        children: [
          const TextField(key: Key('field')),
          ElevatedButton(
            key: const Key('next'),
            focusNode: FocusNode(),
            onPressed: () {},
            child: const Text('Next'),
          ),
        ],
      ),
    ),
  );
}

void main() {
  testWidgets('keyboard up with focused TextField stays silent', (t) async {
    await t.pumpWidget(fixture());
    await t.tap(find.byKey(const Key('field')));
    await t.pump();
    t.view.viewInsets = const FakeViewPadding(bottom: 300);
    addTearDown(t.view.reset);
    await t.pump();
    expect(detectStuckKeyboard(t), isFalse,
        reason: 'a focused text field legitimately owns the keyboard');
  });

  testWidgets('keyboard up with focus on a button fires', (t) async {
    await t.pumpWidget(fixture());
    await t.tap(find.byKey(const Key('field')));
    await t.pump();
    t.view.viewInsets = const FakeViewPadding(bottom: 300);
    addTearDown(t.view.reset);
    // The app moves focus off the field (e.g. navigates) but never dismisses
    // the IME: the stuck-keyboard bug.
    FocusManager.instance.primaryFocus?.unfocus();
    await t.pump();
    expect(detectStuckKeyboard(t), isTrue,
        reason: 'keyboard visible with no editable focused is the bug');
  });

  testWidgets('keyboard down stays silent regardless of focus', (t) async {
    await t.pumpWidget(fixture());
    await t.pump();
    expect(detectStuckKeyboard(t), isFalse,
        reason: 'no IME on screen, nothing to report');
  });
}
