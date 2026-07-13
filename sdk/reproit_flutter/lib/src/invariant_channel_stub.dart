/// Web / no-filesystem fallback for the app-invariant channel.
///
/// The reproit fuzzer signals "you are running under me" by provisioning a
/// marker file and exporting its path as `REPROIT_INVARIANT_FILE`; the SDK
/// appends violated-invariant markers to it (see [ReproIt.invariant]). Neither
/// environment variables nor a filesystem exist on web, so this stub reports
/// "not under the fuzzer" and drops any append. Selected automatically when
/// `dart:io` is unavailable (Flutter web), keeping the SDK web-safe.
library;

/// The runner-provisioned invariant marker file, or null when not under the
/// reproit fuzzer. Always null here (no environment / filesystem on web).
String? invariantFilePath() => null;

/// Append one marker [line] to [path]. No-op on web.
void appendInvariantLine(String path, String line) {}
