/// Native (dart:io) implementation of the app-invariant channel.
///
/// The reproit fuzzer provisions a per-run marker file and exports its path as
/// `REPROIT_INVARIANT_FILE`; that env var is BOTH the SDK's "running under the
/// fuzzer" gate and the channel the explorer scrapes. Under the fuzzer the SDK
/// evaluates the app's registered invariants on each state-settle and APPENDS a
/// `REPROIT_INVARIANT {..}` marker line per violation to this file. Selected
/// automatically when `dart:io` is available (mobile / desktop / the headless
/// `flutter test` tier), so production stays inert when the env var is unset.
library;

import 'dart:io';

/// The runner-provisioned invariant marker file, or null when not under the
/// reproit fuzzer (the `REPROIT_INVARIANT_FILE` env var is unset / empty).
String? invariantFilePath() {
  final p = Platform.environment['REPROIT_INVARIANT_FILE'];
  return (p == null || p.isEmpty) ? null : p;
}

/// Append one marker [line] (plus a newline) to [path]. Best-effort: never lets
/// a filesystem error break the app's frame pipeline.
void appendInvariantLine(String path, String line) {
  try {
    File(path).writeAsStringSync('$line\n', mode: FileMode.append, flush: true);
  } catch (_) {}
}
