part of '../reproit_explorer.dart';

typedef PumpExplorerApp = Future<void> Function(WidgetTester tester);

abstract class ExplorerRuntime {
  const ExplorerRuntime();

  String get testName;

  int get seedStartupMs;

  void emit(String line);

  void startSession(WidgetTester tester) {}

  void finishFrames() {}

  Future<int?> settleAfterTap(
    WidgetTester tester,
    Future<void> Function(WidgetTester tester, int ms) settle,
    int budgetMs,
  );

  void beforeSeed(WidgetTester tester) {}

  void afterFirstPump(WidgetTester tester) {}

  Future<void> afterSeed(WidgetTester tester) async {}

  Future<void> afterRun(
    WidgetTester tester,
    Future<void> Function(WidgetTester tester, int ms) settle,
  ) async {}
}

class SimulatorExplorerRuntime extends ExplorerRuntime {
  const SimulatorExplorerRuntime();

  @override
  String get testName => 'explore';

  @override
  int get seedStartupMs => 2500;

  @override
  void emit(String line) => debugPrint(line);

  @override
  void startSession(WidgetTester tester) => _trackFrames();

  @override
  void finishFrames() => _reportFrames();

  @override
  Future<int?> settleAfterTap(
    WidgetTester tester,
    Future<void> Function(WidgetTester tester, int ms) settle,
    int budgetMs,
  ) => settleWatchdog(tester, budgetMs);

  @override
  Future<void> afterRun(
    WidgetTester tester,
    Future<void> Function(WidgetTester tester, int ms) settle,
  ) async {
    await settle(tester, 1500);
  }
}

class HeadlessExplorerRuntime extends ExplorerRuntime {
  const HeadlessExplorerRuntime();

  @override
  String get testName => 'explore (headless)';

  @override
  int get seedStartupMs => 1500;

  @override
  // Plain stdout avoids the `flutter: ` prefix and preserves marker framing.
  // ignore: avoid_print
  void emit(String line) => print(line);

  @override
  Future<int?> settleAfterTap(
    WidgetTester tester,
    Future<void> Function(WidgetTester tester, int ms) settle,
    int budgetMs,
  ) async {
    await settle(tester, budgetMs);
    _drainException(tester, phase: 'during the walk');
    return null;
  }

  @override
  void beforeSeed(WidgetTester tester) {
    _drainException(tester, phase: 'on teardown of the previous seed');
  }

  @override
  void afterFirstPump(WidgetTester tester) {
    _drainException(tester, phase: 'on first pump');
  }

  @override
  Future<void> afterSeed(WidgetTester tester) async {
    await tester.pumpWidget(const SizedBox.shrink());
    await tester.pump(const Duration(milliseconds: 200));
    _drainException(tester, phase: 'on seed teardown');
  }
}

late ExplorerRuntime _runtime;

void emitJson(String marker, Map<String, dynamic> payload) {
  _runtime.emit('$marker ${jsonEncode(payload)}');
}

final List<List<int>> _frameLog = [];
ui.TimingsCallback? _frameCallback;
int _frameStartMicros = 0;

void _trackFrames() {
  _frameLog.clear();
  _frameStartMicros = 0;
  _frameCallback = (List<ui.FrameTiming> timings) {
    for (final timing in timings) {
      final vsync = timing.timestampInMicroseconds(ui.FramePhase.vsyncStart);
      if (_frameStartMicros == 0) _frameStartMicros = vsync;
      _frameLog.add([
        ((vsync - _frameStartMicros) / 1000).round(),
        timing.buildDuration.inMicroseconds,
        timing.rasterDuration.inMicroseconds,
      ]);
    }
  };
  WidgetsBinding.instance.addTimingsCallback(_frameCallback!);
}

void _reportFrames() {
  final callback = _frameCallback;
  if (callback != null) {
    WidgetsBinding.instance.removeTimingsCallback(callback);
    _frameCallback = null;
  }
  for (var i = 0; i < _frameLog.length; i += 40) {
    final end = (i + 40 > _frameLog.length) ? _frameLog.length : i + 40;
    final chunk = _frameLog
        .sublist(i, end)
        .map((frame) => '${frame[0]},${frame[1]},${frame[2]}')
        .join(';');
    _runtime.emit('FRAMES:BATCH $chunk');
  }
  _runtime.emit('JOURNEY[a] step: recorded ${_frameLog.length} frames');
}

bool _drainException(WidgetTester tester, {String? phase}) {
  final exception = tester.takeException();
  if (exception == null) return false;
  final type = exception.runtimeType.toString();
  final message = exception.toString();
  final frames = RegExp(
    r'(?:package:|file://)[\w./:-]+\.dart:\d+(?::\d+)?',
  ).allMatches(message).map((match) => match.group(0)!).toSet().take(12);
  _runtime.emit(
    '══╡ EXCEPTION CAUGHT BY WIDGETS LIBRARY ╞'
    '═══════════════════════════════════════',
  );
  _runtime.emit(
    'The following $type was thrown${phase != null ? ' $phase' : ''}:',
  );
  for (final line in message.split('\n')) {
    if (line.trim().isEmpty) break;
    _runtime.emit(line);
  }
  _runtime.emit('');
  var index = 0;
  for (final frame in frames) {
    _runtime.emit('#$index      $frame');
    index++;
  }
  _runtime.emit(
    '════════════════'
    '════════════════'
    '════════════════'
    '════════════════',
  );
  return true;
}
