<?php

/*!
 * Minimal test harness for the dependency-free test scripts: `check` records
 * one named assertion, `check_throws` expects a TraceError code, and `report`
 * prints the summary and exits non-zero on any failure.
 */

declare(strict_types=1);

namespace ReproitBackend\Test;

use ReproitBackend\TraceError;

$GLOBALS['reproit_test'] = ['passed' => 0, 'failed' => 0];

function check(bool $condition, string $label): void
{
    if ($condition) {
        $GLOBALS['reproit_test']['passed'] += 1;
        return;
    }
    $GLOBALS['reproit_test']['failed'] += 1;
    fwrite(STDERR, "FAIL: $label\n");
}

function check_same(mixed $expected, mixed $actual, string $label): void
{
    $same = $expected === $actual;
    check($same, $label . ($same ? '' : sprintf(
        ' (expected %s, got %s)',
        var_export($expected, true),
        var_export($actual, true),
    )));
}

function check_throws(callable $callback, string $code, string $label): void
{
    try {
        $callback();
        check(false, $label . ' (nothing thrown)');
    } catch (TraceError $error) {
        check_same($code, $error->getCode(), $label);
    }
}

function report(string $suite): never
{
    $passed = $GLOBALS['reproit_test']['passed'];
    $failed = $GLOBALS['reproit_test']['failed'];
    fwrite(STDOUT, "$suite: $passed passed, $failed failed\n");
    exit($failed === 0 ? 0 : 1);
}
