part of '../reproit_explorer.dart';

class ExplorerNavigation {
  ExplorerNavigation(this.tester);

  final WidgetTester tester;

  Finder? _findByLabel(String label) {
    final isClipped =
        label.length == maxLabelLen && RegExp(r'#[0-9a-f]{8}$').hasMatch(label);
    if (isClipped) {
      final prefix = label.substring(0, label.lastIndexOf('#'));
      final expression = RegExp('^${RegExp.escape(prefix)}');
      var finder = find.bySemanticsLabel(expression);
      if (finder.evaluate().isNotEmpty) return finder;
      finder = find.textContaining(expression);
      if (finder.evaluate().isNotEmpty) return finder;
      return null;
    }
    var finder = find.bySemanticsLabel(label);
    if (finder.evaluate().isNotEmpty) return finder;
    finder = find.bySemanticsLabel(RegExp(RegExp.escape(label)));
    if (finder.evaluate().isNotEmpty) return finder;
    finder = find.text(label);
    if (finder.evaluate().isNotEmpty) return finder;
    return null;
  }

  Future<bool> tapSelector(String selector) async {
    if (selector.startsWith('key:')) {
      final finder = find.byKey(keyFromString(selector.substring(4)));
      if (finder.evaluate().isEmpty) return false;
      try {
        await tester.tap(finder.first, warnIfMissed: false);
        return true;
      } catch (_) {
        return false;
      }
    }
    if (selector.startsWith('role:')) {
      final hash = selector.indexOf('#');
      if (hash < 0) return false;
      final role = selector.substring('role:'.length, hash);
      final index = int.tryParse(selector.substring(hash + 1)) ?? -1;
      if (index < 0) return false;
      var seen = -1;
      SemanticsNode? target;
      final root = _semanticsRoot(tester);
      if (root != null) {
        void walk(SemanticsNode node) {
          if (target != null) return;
          final data = node.getSemanticsData();
          if (!data.flagsCollection.isHidden) {
            final tappable =
                data.hasAction(SemanticsAction.tap) &&
                !data.flagsCollection.isTextField;
            if (tappable && roleOf(data) == role) {
              seen++;
              if (seen == index) target = node;
            }
          }
          node.visitChildren((child) {
            walk(child);
            return true;
          });
        }

        walk(root);
      }
      if (target == null) return false;
      try {
        tester.semantics.tap(
          find.semantics.byPredicate((node) => node == target),
        );
        return true;
      } catch (_) {
        return false;
      }
    }
    final label = selector.startsWith('label:')
        ? selector.substring('label:'.length)
        : selector;
    final finder = _findByLabel(label);
    if (finder == null) return false;
    try {
      await tester.tap(finder.first, warnIfMissed: false);
      return true;
    } catch (_) {
      return false;
    }
  }

  Future<bool> goBack() async {
    try {
      final navigator = tester.state<NavigatorState>(
        find.byType(Navigator).first,
      );
      final popped = await navigator.maybePop();
      await settleExplorer(tester, 900);
      return popped;
    } catch (_) {
      return false;
    }
  }
}
