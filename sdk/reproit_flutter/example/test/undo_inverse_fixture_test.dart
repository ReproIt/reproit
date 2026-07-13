// Dogfood for the UNDO-INVERSE oracle (templates/explorer.dart +
// explorer_headless.dart :: isToggleRole/undoAffordanceSel/undoInverseVerdict and
// the do-then-undo drive in the walk). The helpers below are a PARITY COPY of the
// template functions (templates cannot be imported); if the template logic changes,
// change it here too. Validates the whole metamorphic relation live, both
// directions and both inverses:
//   1. a boolean TOGGLE whose re-tap does not restore the structure   -> fires
//   2. a toggle that round-trips cleanly                              -> silent
//   3. an UNDO affordance that leaves residue                         -> fires
//   4. an undo that fully restores the prior structure               -> silent
//   5. a DEAD re-tap of a toggle is inconclusive (never a false fire) -> skip
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

// PARITY COPY of templates/explorer.dart::isToggleRole.
bool isToggleRole(String role) => role == 'switch' || role == 'checkbox';

// PARITY COPY of templates/explorer.dart::undoInverseVerdict.
String undoInverseVerdict(String beforeSig, String postSig, String afterSig,
    {required bool viaUndo}) {
  if (afterSig == beforeSig) return 'clean';
  if (!viaUndo && afterSig == postSig) return 'skip';
  return 'fire';
}

// A structural signature stand-in over the fixture: the number of list items plus
// whether a residual control is present. Mirrors the template's canonical signature
// in the only way that matters here -- it moves when the STRUCTURE changes and is
// invariant to value/text. (The template signs the real semantics tree; the fixture
// keeps a small, deterministic structural proxy so the test needs no template.)
String structSig(WidgetTester t) {
  final items = find.byType(_Item).evaluate().length;
  final residue = find.byKey(const Key('residue')).evaluate().isNotEmpty;
  return 'items:$items|residue:$residue';
}

// The label of the reachable Undo affordance, if any (parity with
// undoAffordanceSel, resolved here by visible text over the live tree).
final RegExp _undoWordRe = RegExp(r'\bundo\b', caseSensitive: false);
bool hasUndoAffordance(WidgetTester t) => find
    .byWidgetPredicate((w) => w is Text && w.data != null && _undoWordRe.hasMatch(w.data!))
    .evaluate()
    .isNotEmpty;

class _Item extends StatelessWidget {
  const _Item(this.text, {super.key});
  final String text;
  @override
  Widget build(BuildContext context) => Text(text);
}

/// A checkbox that reveals a 3-item panel when checked. When unchecked, a correct
/// app clears the panel; the buggy app leaves ONE residual item behind.
class ToggleFixture extends StatefulWidget {
  const ToggleFixture({super.key, required this.leaveResidue});
  final bool leaveResidue;
  @override
  State<ToggleFixture> createState() => _ToggleFixtureState();
}

class _ToggleFixtureState extends State<ToggleFixture> {
  bool _on = false;
  bool _cycledOff = false; // has the toggle been switched OFF at least once
  @override
  Widget build(BuildContext context) {
    final items = <Widget>[];
    if (_on) {
      items.addAll(const [_Item('A'), _Item('B'), _Item('C')]);
    } else if (widget.leaveResidue && _cycledOff) {
      // BUG: switching OFF leaves one residual item behind (the initial OFF
      // state, before any cycle, is clean -- so X genuinely adds structure).
      items.add(const _Item('residue', key: Key('residue')));
    }
    return MaterialApp(
      home: Scaffold(
        body: Column(children: [
          Checkbox(
              value: _on,
              onChanged: (v) => setState(() {
                    _on = v ?? false;
                    if (!_on) _cycledOff = true;
                  })),
          ...items,
        ]),
      ),
    );
  }
}

/// A list with a Delete control. Deleting removes the item and shows an Undo
/// control (the snackbar pattern, modeled in-tree). Undo restores the item; the
/// buggy app also leaves a residual control behind.
class UndoFixture extends StatefulWidget {
  const UndoFixture({super.key, required this.leaveResidue});
  final bool leaveResidue;
  @override
  State<UndoFixture> createState() => _UndoFixtureState();
}

class _UndoFixtureState extends State<UndoFixture> {
  bool _present = true;
  bool _deleted = false;
  bool _residue = false;
  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      home: Scaffold(
        body: Column(children: [
          if (_present) const _Item('Message 1'),
          if (_present)
            TextButton(
              onPressed: () => setState(() {
                _present = false;
                _deleted = true;
              }),
              child: const Text('Delete'),
            ),
          if (_deleted)
            TextButton(
              onPressed: () => setState(() {
                _present = true;
                _deleted = false;
                _residue = widget.leaveResidue;
              }),
              child: const Text('Undo'),
            ),
          if (_residue)
            const _Item('leftover', key: Key('residue')),
        ]),
      ),
    );
  }
}

void main() {
  test('the verdict is inconclusive on a dead re-tap (never a false fire)', () {
    // A re-tapped toggle that did nothing (afterSig == postSig) is not asserted.
    expect(undoInverseVerdict('a', 'b', 'b', viaUndo: false), 'skip');
    // A clean round-trip is silent; residue fires.
    expect(undoInverseVerdict('a', 'b', 'a', viaUndo: false), 'clean');
    expect(undoInverseVerdict('a', 'b', 'c', viaUndo: false), 'fire');
    // A dead UNDO that leaves the post-X structure IS a failure to restore.
    expect(undoInverseVerdict('a', 'b', 'b', viaUndo: true), 'fire');
    // Only genuine 2-state controls are treated as re-tappable inverses.
    expect(isToggleRole('checkbox'), isTrue);
    expect(isToggleRole('switch'), isTrue);
    expect(isToggleRole('button'), isFalse);
  });

  testWidgets('a toggle that leaves residue fires', (t) async {
    await t.pumpWidget(const ToggleFixture(leaveResidue: true));
    await t.pumpAndSettle();
    final before = structSig(t);
    await t.tap(find.byType(Checkbox)); // X
    await t.pumpAndSettle();
    final post = structSig(t);
    expect(post, isNot(before), reason: 'X changed the structure');
    await t.tap(find.byType(Checkbox)); // inverse (re-tap the 2-state toggle)
    await t.pumpAndSettle();
    final after = structSig(t);
    expect(undoInverseVerdict(before, post, after, viaUndo: false), 'fire',
        reason: 'the toggle did not round-trip: $before -> $post -> $after');
  });

  testWidgets('a toggle that round-trips cleanly stays silent', (t) async {
    await t.pumpWidget(const ToggleFixture(leaveResidue: false));
    await t.pumpAndSettle();
    final before = structSig(t);
    await t.tap(find.byType(Checkbox));
    await t.pumpAndSettle();
    final post = structSig(t);
    await t.tap(find.byType(Checkbox));
    await t.pumpAndSettle();
    final after = structSig(t);
    expect(undoInverseVerdict(before, post, after, viaUndo: false), 'clean',
        reason: 'a correct toggle restores the prior structure');
  });

  testWidgets('an undo affordance that leaves residue fires', (t) async {
    await t.pumpWidget(const UndoFixture(leaveResidue: true));
    await t.pumpAndSettle();
    final hadUndoBefore = hasUndoAffordance(t);
    expect(hadUndoBefore, isFalse, reason: 'no Undo exists before X');
    final before = structSig(t);
    await t.tap(find.text('Delete')); // X
    await t.pumpAndSettle();
    final post = structSig(t);
    expect(hasUndoAffordance(t), isTrue, reason: 'X created an Undo affordance');
    await t.tap(find.text('Undo')); // the inverse that appeared because of X
    await t.pumpAndSettle();
    final after = structSig(t);
    expect(undoInverseVerdict(before, post, after, viaUndo: true), 'fire',
        reason: 'undo left residue: $before -> $post -> $after');
  });

  testWidgets('an undo that fully restores stays silent', (t) async {
    await t.pumpWidget(const UndoFixture(leaveResidue: false));
    await t.pumpAndSettle();
    final before = structSig(t);
    await t.tap(find.text('Delete'));
    await t.pumpAndSettle();
    final post = structSig(t);
    await t.tap(find.text('Undo'));
    await t.pumpAndSettle();
    final after = structSig(t);
    expect(undoInverseVerdict(before, post, after, viaUndo: true), 'clean',
        reason: 'a correct undo restores the prior structure');
  });
}
