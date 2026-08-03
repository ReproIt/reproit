part of '../reproit_explorer.dart';

typedef PumpExplorerApp = Future<void> Function(WidgetTester tester);

abstract class ExplorerRuntime {
  const ExplorerRuntime();

  String get testName;

  int get seedStartupMs;

  void emit(String line);

  void startSession(WidgetTester tester) {}

  void finishFrames() {}

  Future<void> beginTransitionFrames(WidgetTester tester) async {}

  Map<String, int>? finishTransitionJank() => null;

  Map<String, dynamic>? finishTransitionFlicker() => null;

  List<Map<String, dynamic>> finishTransitionOverflows() => const [];

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
  void startSession(WidgetTester tester) {
    _trackFrames();
    _trackFrameworkOverflows();
  }

  @override
  void finishFrames() {
    _stopFrameworkOverflows();
    _reportFrames();
  }

  @override
  Future<void> beginTransitionFrames(WidgetTester tester) =>
      _beginTransitionFrames(tester);

  @override
  Map<String, int>? finishTransitionJank() => _finishTransitionJank();

  @override
  Map<String, dynamic>? finishTransitionFlicker() => _finishTransitionFlicker();

  @override
  List<Map<String, dynamic>> finishTransitionOverflows() =>
      _finishTransitionOverflows();

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
int _transitionFrameStart = 0;
final List<Uint8List> _transitionPixels = [];
final List<Map<String, dynamic>> _frameworkOverflows = [];
void Function(FlutterErrorDetails)? _previousFlutterErrorHandler;
int _transitionOverflowStart = 0;

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

Future<void> _beginTransitionFrames(WidgetTester tester) async {
  _transitionFrameStart = _frameLog.length;
  _transitionOverflowStart = _frameworkOverflows.length;
  _transitionPixels.clear();
  await _captureTransitionFrame(tester);
}

Map<String, dynamic>? classifyFrameworkOverflow(String message) {
  final match = RegExp(
    r'A RenderFlex overflowed by ([0-9]+(?:\.[0-9]+)?) pixels on the '
    r'(left|right|top|bottom)\.',
  ).firstMatch(message);
  if (match == null) return null;
  final pixels = double.tryParse(match.group(1)!);
  if (pixels == null || !pixels.isFinite || pixels <= 0) return null;
  return {'key': 'render-flex', 'edge': match.group(2)!, 'by': pixels.ceil()};
}

void _trackFrameworkOverflows() {
  _frameworkOverflows.clear();
  _previousFlutterErrorHandler = FlutterError.onError;
  FlutterError.onError = (details) {
    final item = classifyFrameworkOverflow(details.exceptionAsString());
    if (item != null && _frameworkOverflows.length < 20) {
      _frameworkOverflows.add(item);
    }
    _previousFlutterErrorHandler?.call(details);
  };
}

void _stopFrameworkOverflows() {
  if (_previousFlutterErrorHandler != null) {
    FlutterError.onError = _previousFlutterErrorHandler;
    _previousFlutterErrorHandler = null;
  }
}

List<Map<String, dynamic>> _finishTransitionOverflows() {
  final start = _transitionOverflowStart.clamp(0, _frameworkOverflows.length);
  final unique = <String, Map<String, dynamic>>{};
  for (final item in _frameworkOverflows.skip(start)) {
    unique['${item['key']}|${item['edge']}'] = item;
  }
  return unique.values.toList()..sort(
    (left, right) =>
        (left['edge'] as String).compareTo(right['edge'] as String),
  );
}

Future<void> _captureTransitionFrame(WidgetTester tester) async {
  if (_transitionPixels.length >= 16) return;
  try {
    final renderView = tester.binding.renderViews.first;
    final layer = renderView.debugLayer;
    if (layer is! OffsetLayer) return;
    final data = await tester.binding.runAsync<ByteData?>(() async {
      final image = await layer.toImage(
        renderView.paintBounds,
        pixelRatio: 0.25,
      );
      try {
        return image.toByteData(format: ui.ImageByteFormat.rawRgba);
      } finally {
        image.dispose();
      }
    });
    if (data != null) _transitionPixels.add(data.buffer.asUint8List());
  } catch (error) {
    assert(() {
      debugPrint('ReproIt frame capture unavailable: $error');
      return true;
    }());
    // A profile binding without a readable layer has no frame authority.
  }
}

double _pixelDifference(Uint8List left, Uint8List right) {
  if (left.length != right.length || left.isEmpty) return 1.0;
  var changed = 0;
  final pixels = left.length ~/ 4;
  for (var offset = 0; offset + 3 < left.length; offset += 4) {
    final delta =
        (left[offset] - right[offset]).abs() +
        (left[offset + 1] - right[offset + 1]).abs() +
        (left[offset + 2] - right[offset + 2]).abs();
    if (delta > 48) changed++;
  }
  return changed / pixels;
}

Map<String, dynamic>? classifyTransitionFlicker(List<Uint8List> frames) {
  if (frames.length < 3) return null;
  final end = frames.last;
  final start = _pixelDifference(frames.first, end);
  var peak = 0.0;
  for (final frame in frames.skip(1).take(frames.length - 2)) {
    final difference = _pixelDifference(frame, end);
    if (difference > peak) peak = difference;
  }
  if (peak <= 0.04 || peak <= (start > 0.04 ? start : 0.04) * 1.35) {
    return null;
  }
  return {'peak': (peak * 1000).round() / 1000, 'frames': frames.length};
}

Map<String, dynamic>? _finishTransitionFlicker() =>
    classifyTransitionFlicker(_transitionPixels);

/// Conservative frame-jank classifier over real engine timings. A single
/// ordinary slow frame abstains because shader compilation and simulator host
/// scheduling can produce one. A finding needs either one presentation stall
/// at least 350ms, or two frames at least 100ms in the same transition. Build
/// and raster are summed because Flutter runs them in distinct pipeline phases.
/// The emitted bucket is fixed, so timing noise cannot change finding identity.
Map<String, int>? classifyTransitionJank(List<List<int>> frames) {
  if (frames.isEmpty) return null;
  var severe = 0;
  var long = 0;
  for (final frame in frames.take(240)) {
    if (frame.length < 3) continue;
    final totalMicros = frame[1] + frame[2];
    if (totalMicros >= 350000) severe++;
    if (totalMicros >= 100000) long++;
  }
  if (severe == 0 && long < 2) return null;
  return {'bucket': 100, 'count': severe > 0 ? severe : long};
}

Map<String, int>? _finishTransitionJank() {
  final start = _transitionFrameStart.clamp(0, _frameLog.length);
  return classifyTransitionJank(_frameLog.sublist(start));
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
