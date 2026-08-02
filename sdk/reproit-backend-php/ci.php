<?php

/*!
 * CI capture mode for reproit-backend-php: the flaky-CI wedge.
 *
 * `Ci::suite($name)` returns a `$test($name, $fn)` callable for the plain
 * test scripts this SDK itself uses (one file, run directly with
 * `php test/x_test.php`; no PHPUnit dependency, named as a follow-up gap in
 * the README). The trigger identity is the TEST (suite + test id), not an
 * inbound HTTP request. With `REPROIT_CI_CAPTURE=1` every test runs inside
 * its own trace as the ambient `Instrument` trace, so the explicit outbound
 * boundary (`Instrument::http`, `RecordingClient`, `RecordingPdo`) records
 * dependency exchanges and the determinism envelope exactly as production
 * capture does; a FAILING test spools a version-2 `reproit-backend-capture`
 * capsule to a bounded on-disk spool. With `REPROIT_REPLAY` set the SAME
 * wrapper re-runs only the capsule's named test while the SDK serves the
 * recorded exchanges in process, and reports the observed result as a
 * structured stderr marker for `reproit check`. Without either env the
 * wrapper just runs the test and keeps the script's exit code honest.
 *
 * The wire is the existing capture payload: the test identity rides in the
 * `operation` field as `test:<suite>#<test>`, and the failed assertion is
 * the existing `backend-authored-invariant` registry oracle (a test IS an
 * authored invariant). No new protocol fields, no new oracle ids.
 *
 * Process model: the spool write happens synchronously in the catch, and a
 * shutdown function (`register_shutdown_function`, the same seam capture.php
 * uses) spools the in-flight test when a fatal error kills the request-scoped
 * process before the catch can run. Failure is counted, never masked.
 *
 * Honest limit: replay pins the envelope and the recorded exchanges, which
 * is the whole boundary this SDK can see. A race the boundary cannot see
 * (scheduling, shared memory) is not reproduced by this capsule; `reproit
 * check` reports such runs Inconclusive, never a fake reproduction.
 */

declare(strict_types=1);

namespace ReproitBackend;

require_once __DIR__ . '/trace.php';
require_once __DIR__ . '/capture.php';
require_once __DIR__ . '/instrument.php';

// Test-trigger identity prefix inside the existing `operation` field.
const TEST_TRIGGER_PREFIX = 'test:';
// The registry oracle a failed test capsule carries: an authored invariant
// (the test's own assertion) was violated. Existing id, not a new one.
const TEST_FAILURE_ORACLE = 'backend-authored-invariant';
// Structured stderr markers `reproit check` parses, like REPROIT:DIVERGENCE.
const CI_RESULT_MARKER = 'REPROIT:CI-TEST ';
const CI_SPOOL_MARKER = 'REPROIT:CI-CAPSULE ';

// Spool bounds. The cap covers the TOTAL bytes on disk; capsules beyond it
// are dropped and counted (in-process stats plus the on-disk
// `dropped.count`), never silently.
const CI_DEFAULT_SPOOL_DIR = '.reproit/ci-spool';
const CI_DEFAULT_SPOOL_MAX_BYTES = 16 * 1024 * 1024;
const CI_SPOOL_MAX_FLOOR_BYTES = 4 * 1024;
const CI_SPOOL_MAX_CEIL_BYTES = 64 * 1024 * 1024;
// Suite and test names share the operation field's 256-code-point bound.
const CI_MAX_NAME = 120;
const CI_MAX_ERROR_CHARS = 2048;

final class Ci
{
    private static int $traceSeq = 1;
    private static int $failed = 0;
    private static int $passed = 0;
    private static bool $hooksRegistered = false;
    /** The capture-mode test currently running: [trace, operation]. */
    private static ?array $inFlight = null;
    private static array $stats = [
        'spooledCapsules' => 0,
        'droppedCapsules' => 0,
        'failedCaptures' => 0,
    ];

    /**
     * The test wrapper for `$suiteName`, mode decided by env at call time:
     * REPROIT_REPLAY wins, then REPROIT_CI_CAPTURE=1, else plain execution.
     * `$options` is reserved; unknown keys are rejected so a typo cannot
     * silently change capture behavior.
     */
    public static function suite(string $suiteName, array $options = []): callable
    {
        if ($options !== []) {
            throw new \InvalidArgumentException(
                'reproit Ci::suite: unknown option ' . (string) array_key_first($options)
            );
        }
        self::registerHooks();
        if (self::replayPath() !== null) {
            return self::replayTest($suiteName);
        }
        if (getenv('REPROIT_CI_CAPTURE') === '1') {
            return self::captureTest($suiteName);
        }
        return function (string $testName, callable $fn): void {
            try {
                $fn();
            } catch (\Throwable $error) {
                self::testFailed($testName, $error);
                return;
            }
            self::$passed += 1;
        };
    }

    /** @return array<string, int> */
    public static function stats(): array
    {
        return self::$stats;
    }

    private static function replayPath(): ?string
    {
        $value = getenv('REPROIT_REPLAY');
        return \is_string($value) && $value !== '' ? $value : null;
    }

    private static function boundedName(string $value): string
    {
        return codepoint_slice(trim($value), CI_MAX_NAME);
    }

    private static function operationFor(string $suiteName, string $testName): string
    {
        return TEST_TRIGGER_PREFIX . self::boundedName($suiteName)
            . '#' . self::boundedName($testName);
    }

    private static function boundedError(\Throwable|string $error): string
    {
        $message = \is_string($error) ? $error : $error->getMessage();
        return codepoint_slice($message, CI_MAX_ERROR_CHARS);
    }

    private static function testFailed(string $testName, \Throwable|string $error): void
    {
        self::$failed += 1;
        $message = \is_string($error) ? $error : $error->getMessage();
        fwrite(STDERR, 'FAIL: ' . $testName . ': ' . $message . "\n");
    }

    /**
     * Two shutdown hooks, registered once, in this order: the fatal-error
     * safety net first (a fatal mid-test must spool before anything exits),
     * then the exit-code hook, because `exit()` inside a shutdown function
     * stops the remaining ones.
     */
    private static function registerHooks(): void
    {
        if (self::$hooksRegistered) {
            return;
        }
        self::$hooksRegistered = true;
        register_shutdown_function(static function (): void {
            if (self::$inFlight === null) {
                return;
            }
            [$trace, $operation] = self::$inFlight;
            self::$inFlight = null;
            $last = error_get_last();
            $message = \is_array($last) && \is_string($last['message'] ?? null)
                ? $last['message']
                : 'process exited mid-test';
            self::finishAndSpool($trace, $operation, $message);
        });
        register_shutdown_function(static function (): void {
            $summary = 'ci: ' . self::$passed . ' passed, ' . self::$failed . " failed\n";
            fwrite(STDOUT, $summary);
            if (self::$failed > 0 && self::$inFlight === null) {
                exit(1);
            }
        });
    }

    /** Synthesized trace context: the CI job stands where production stood. */
    private static function ciContext(): array
    {
        $commit = null;
        foreach ([getenv('REPROIT_COMMIT') ?: null, getenv('GITHUB_SHA') ?: null] as $value) {
            if (valid_token($value)) {
                $commit = (string) $value;
                break;
            }
        }
        return [
            'traceId' => 'ci-' . (int) (microtime(true) * 1000) . '-' . self::$traceSeq++,
            'actor' => null,
            'actionIndex' => 0,
            'build' => $commit,
            'configContract' => null,
            'captureEnvelope' => true,
        ];
    }

    private static function spoolDir(): string
    {
        $dir = getenv('REPROIT_CI_SPOOL');
        return \is_string($dir) && $dir !== '' ? $dir : CI_DEFAULT_SPOOL_DIR;
    }

    private static function spoolMaxBytes(): int
    {
        $raw = getenv('REPROIT_CI_SPOOL_MAX');
        $parsed = \is_string($raw) ? filter_var($raw, FILTER_VALIDATE_INT) : false;
        if (!\is_int($parsed)) {
            return CI_DEFAULT_SPOOL_MAX_BYTES;
        }
        return min(CI_SPOOL_MAX_CEIL_BYTES, max(CI_SPOOL_MAX_FLOOR_BYTES, $parsed));
    }

    private static function recordDrop(string $dir): void
    {
        $counter = $dir . '/dropped.count';
        $dropped = (int) @file_get_contents($counter);
        @file_put_contents($counter, ($dropped + 1) . "\n");
    }

    /**
     * Write one capsule inside the byte cap; over-cap capsules are dropped
     * and counted. Returns the file path or null.
     */
    private static function spool(array $payload): ?string
    {
        $body = canonical_json($payload);
        $bytes = \strlen($body);
        $dir = self::spoolDir();
        if (!is_dir($dir)) {
            @mkdir($dir, 0o777, true);
        }
        $used = 0;
        foreach (glob($dir . '/*.json') ?: [] as $entry) {
            // A concurrently removed entry counts as zero.
            $used += (int) @filesize($entry);
        }
        if ($used + $bytes > self::spoolMaxBytes()) {
            self::$stats['droppedCapsules'] += 1;
            self::recordDrop($dir);
            return null;
        }
        $file = $dir . '/capsule-' . substr(hash('sha256', $body), 0, 12) . '.json';
        file_put_contents($file, $body);
        self::$stats['spooledCapsules'] += 1;
        fwrite(STDERR, CI_SPOOL_MARKER . marker_json([
            'file' => $file,
            'operation' => $payload['operation'],
        ]) . "\n");
        return $file;
    }

    private static function finishAndSpool(
        BackendTrace $trace,
        string $operation,
        string $error
    ): void {
        try {
            $bounded = codepoint_slice($error, CI_MAX_ERROR_CHARS);
            $trace->finish(['error' => $bounded], null, false, false);
            $events = $trace->events();
            $first = $events[0] ?? [];
            self::spool([
                'format' => CAPTURE_FORMAT,
                'version' => CAPTURE_VERSION_EXCHANGES,
                'operation' => $operation,
                'oracle' => TEST_FAILURE_ORACLE,
                // Same envelope shape production capture records; the seed
                // pins the REPLAY run's randomness, not the test run's.
                'envelope' => determinism_envelope(
                    \is_int($first['at'] ?? null) ? $first['at'] : null
                ),
                'events' => $events,
            ]);
        } catch (\Throwable $ignored) {
            // Capture must never mask the test's own failure.
            self::$stats['failedCaptures'] += 1;
        }
    }

    private static function captureTest(string $suiteName): callable
    {
        return function (string $testName, callable $fn) use ($suiteName): void {
            $operation = self::operationFor($suiteName, $testName);
            $trace = BackendTrace::begin(self::ciContext(), $operation, [
                'input' => [
                    'suite' => self::boundedName($suiteName),
                    'test' => self::boundedName($testName),
                ],
            ]);
            Instrument::setTrace($trace);
            self::$inFlight = [$trace, $operation];
            try {
                $fn();
            } catch (\Throwable $error) {
                self::$inFlight = null;
                Instrument::setTrace(null);
                self::finishAndSpool($trace, $operation, self::boundedError($error));
                self::testFailed($testName, $error);
                return;
            }
            self::$inFlight = null;
            Instrument::setTrace(null);
            try {
                $trace->finish(null, null, true, false);
            } catch (\Throwable $ignored) {
                // An over-long passing trace has nothing to spool anyway.
            }
            self::$passed += 1;
        };
    }

    /**
     * The capsule names exactly one test; everything else is skipped so the
     * process exit code speaks for the named test alone.
     */
    private static function replayTarget(): string
    {
        $raw = @file_get_contents((string) self::replayPath());
        $payload = $raw === false ? null : json_decode($raw, true);
        $operation = \is_array($payload) ? ($payload['operation'] ?? null) : null;
        if (!\is_string($operation) || !str_starts_with($operation, TEST_TRIGGER_PREFIX)) {
            throw new \RuntimeException(
                'REPROIT_REPLAY capsule does not carry a test trigger identity'
            );
        }
        return $operation;
    }

    private static function reportResult(string $operation, string $status, ?string $failure): void
    {
        $detail = ['operation' => $operation, 'status' => $status];
        if ($failure !== null) {
            $detail['failure'] = $failure;
        }
        fwrite(STDERR, CI_RESULT_MARKER . marker_json($detail) . "\n");
    }

    private static function replayTest(string $suiteName): callable
    {
        // Load the session now so the envelope (timezone, seeded mt_rand) is
        // pinned before any test code runs, like production replay boot.
        Instrument::session();
        $target = self::replayTarget();
        return function (string $testName, callable $fn) use ($suiteName, $target): void {
            $operation = self::operationFor($suiteName, $testName);
            if ($operation !== $target) {
                return; // reproit replay targets exactly one named test
            }
            try {
                $fn();
            } catch (\Throwable $error) {
                self::reportResult($operation, 'failed', self::boundedError($error));
                self::$failed += 1;
                return;
            }
            self::reportResult($operation, 'passed', null);
            self::$passed += 1;
        };
    }

    /** Test seam: forget counters and hooks state between in-process runs. */
    public static function resetForTests(): void
    {
        self::$failed = 0;
        self::$passed = 0;
        self::$inFlight = null;
        self::$stats = [
            'spooledCapsules' => 0,
            'droppedCapsules' => 0,
            'failedCaptures' => 0,
        ];
    }
}
