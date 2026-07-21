part of '../reproit_explorer.dart';

Future<void> settleExplorer(WidgetTester tester, int milliseconds) async {
  for (var elapsed = 0; elapsed < milliseconds; elapsed += 100) {
    await tester.pump(const Duration(milliseconds: 100));
  }
}

Future<bool> waitForExplorer(
  WidgetTester tester,
  bool Function() predicate,
) async {
  final stopwatch = Stopwatch()..start();
  while (stopwatch.elapsed < const Duration(seconds: 8)) {
    if (predicate()) return true;
    await Future.delayed(const Duration(milliseconds: 250));
    await tester.pump(const Duration(milliseconds: 100));
  }
  return predicate();
}
