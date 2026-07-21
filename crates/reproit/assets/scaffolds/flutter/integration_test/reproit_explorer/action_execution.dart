part of '../reproit_explorer.dart';

class ExplorerActions {
  ExplorerActions(this.tester, this.runtime)
    : navigation = ExplorerNavigation(tester);

  final WidgetTester tester;
  final ExplorerRuntime runtime;
  final ExplorerNavigation navigation;

  Future<bool> fillField(String field, String value) async {
    for (final finder in [
      find.bySemanticsLabel(field),
      find.bySemanticsLabel(RegExp(RegExp.escape(field))),
    ]) {
      if (finder.evaluate().isNotEmpty) {
        try {
          await tester.enterText(finder.first, value);
          await settleExplorer(tester, 500);
          return true;
        } catch (_) {}
      }
    }
    var edits = find.byType(EditableText).hitTestable();
    if (edits.evaluate().isEmpty) edits = find.byType(EditableText);
    final digits = field.replaceAll(RegExp(r'[^0-9]'), '');
    final index = int.tryParse(digits);
    if (index != null && index < edits.evaluate().length) {
      try {
        await tester.enterText(edits.at(index), value);
        await settleExplorer(tester, 500);
        return true;
      } catch (_) {}
    }
    return false;
  }

  bool textPresent(String text) =>
      find.textContaining(text).evaluate().isNotEmpty ||
      find.bySemanticsLabel(RegExp(RegExp.escape(text))).evaluate().isNotEmpty;

  int countMatching(String selector) {
    if (selector.startsWith('key:')) {
      return find.byKey(keyFromString(selector.substring(4))).evaluate().length;
    }
    if (selector.startsWith('role:')) {
      final hash = selector.indexOf('#');
      final role = selector.substring(
        'role:'.length,
        hash < 0 ? selector.length : hash,
      );
      var count = 0;
      final root = _semanticsRoot(tester);
      if (root != null) {
        void walk(SemanticsNode node) {
          final data = node.getSemanticsData();
          if (!data.flagsCollection.isHidden && roleOf(data) == role) count++;
          node.visitChildren((child) {
            walk(child);
            return true;
          });
        }

        walk(root);
      }
      return count;
    }
    return find.textContaining(selector).evaluate().length;
  }

  Future<bool> fillSelector(String selector, String value) async {
    if (selector.startsWith('key:')) {
      final finder = find.byKey(keyFromString(selector.substring(4)));
      if (finder.evaluate().isEmpty) return false;
      try {
        await tester.enterText(finder.first, value);
        await settleExplorer(tester, 500);
        return true;
      } catch (_) {
        return false;
      }
    }
    return fillField(selector, value);
  }

  Future<void> executeAssertion(String specification, String actor) async {
    if (specification.startsWith('state=')) {
      final expected = specification.substring('state='.length);
      final passed = await waitForExplorer(
        tester,
        () => snapshot(tester).sig == expected,
      );
      final actual = snapshot(tester).sig;
      runtime.emit(
        'FUZZ:ASSERT ${passed ? "pass" : "fail"} state=$expected '
        'got=$actual actor=$actor',
      );
      return;
    }
    if (specification.startsWith('route=')) {
      final expected = specification.substring('route='.length);
      final passed = await waitForExplorer(
        tester,
        () => snapshot(tester).anchor == expected,
      );
      final actual = snapshot(tester).anchor ?? '';
      runtime.emit(
        'FUZZ:ASSERT ${passed ? "pass" : "fail"} route=$expected '
        'got=$actual actor=$actor',
      );
      return;
    }
    if (specification.startsWith('text=')) {
      final expected = specification.substring('text='.length);
      final passed = await waitForExplorer(tester, () => textPresent(expected));
      runtime.emit(
        'FUZZ:ASSERT ${passed ? "pass" : "fail"} '
        'text=${jsonEncode(expected)} actor=$actor',
      );
      return;
    }
    if (specification.startsWith('count:')) {
      final remainder = specification.substring('count:'.length);
      final equals = remainder.lastIndexOf('=');
      final selector = equals >= 0 ? remainder.substring(0, equals) : remainder;
      final expected = equals >= 0
          ? (int.tryParse(remainder.substring(equals + 1)) ?? 0)
          : 0;
      final passed = await waitForExplorer(
        tester,
        () => countMatching(selector) == expected,
      );
      final actual = countMatching(selector);
      runtime.emit(
        'FUZZ:ASSERT ${passed ? "pass" : "fail"} count $selector '
        'want=$expected got=$actual actor=$actor',
      );
    }
  }

  Future<void> executeScenario(String action, String actor) async {
    runtime.emit('FUZZ:ACT $actor $action');
    if (action == 'back') {
      await navigation.goBack();
      return;
    }
    if (action.startsWith('auth:')) {
      runtime.emit(
        'JOURNEY[a] step: auth-restore unsupported on flutter runner; '
        'use login() for $action',
      );
      await settleExplorer(tester, 200);
      return;
    }
    if (action.startsWith('assert:')) {
      await executeAssertion(action.substring('assert:'.length), actor);
      return;
    }
    if (action.startsWith('type:')) {
      final body = action.substring('type:'.length);
      final equals = body.lastIndexOf('=');
      final selector = equals >= 0 ? body.substring(0, equals) : body;
      final value = equals >= 0 ? body.substring(equals + 1) : '';
      var passed = await fillSelector(selector, value);
      if (!passed) {
        passed =
            await waitForExplorer(tester, () => countMatching(selector) > 0) &&
            await fillSelector(selector, value);
      }
      if (!passed) runtime.emit('FUZZ:MISS $actor $action');
      return;
    }
    final selector = action.startsWith('tap:')
        ? action.substring('tap:'.length)
        : action;
    var passed = await navigation.tapSelector(selector);
    final stopwatch = Stopwatch()..start();
    while (!passed && stopwatch.elapsed < const Duration(seconds: 8)) {
      await Future.delayed(const Duration(milliseconds: 250));
      await tester.pump(const Duration(milliseconds: 100));
      passed = await navigation.tapSelector(selector);
    }
    if (!passed) runtime.emit('FUZZ:MISS $actor $action');
    await settleExplorer(tester, 1000);
  }
}
