<?php

declare(strict_types=1);

/*
 * Shared behavioral conformance vectors (sdk/capture-behavior-v1.json).
 *
 * Eleven SDKs hand implement one contract, so a defect otherwise has to be
 * found eleven times. Four instances of one class landed in a single day, and
 * every group here was written against one of them.
 */

require_once __DIR__ . '/../reproit.php';

$vectors = json_decode(
    (string) file_get_contents(__DIR__ . '/../../capture-behavior-v1.json'),
    true,
    512,
    JSON_THROW_ON_ERROR
);

$passed = 0;
$failed = 0;

function check(string $label, bool $condition, string $detail = ''): void
{
    global $passed, $failed;
    if ($condition) {
        $passed++;
        return;
    }
    $failed++;
    fwrite(STDERR, "FAIL {$label}" . ($detail === '' ? '' : ": {$detail}") . "\n");
}

// Constants
check(
    'body bound matches vectors',
    \ReproitBackend\MAX_EXCHANGE_BODY_BYTES === $vectors['constants']['maxExchangeBodyBytes']
);
check(
    'divergence marker matches vectors',
    \ReproitBackend\DIVERGENCE_MARKER === $vectors['constants']['divergenceMarker']
);

// Redaction, type cases
foreach ($vectors['redaction']['typeCases'] as $case) {
    $actual = \ReproitBackend\redact($case['input']);
    check(
        'redaction type ' . json_encode($case['input']),
        json_encode($actual) === json_encode($case['expect']),
        'got ' . json_encode($actual) . ' want ' . json_encode($case['expect'])
    );
}

// Redaction, key folding
foreach ($vectors['redaction']['foldingCases'] as $case) {
    $out = \ReproitBackend\redact([$case['field'] => 'value']);
    $value = $out[$case['field']];
    $redacted = is_array($value) && array_key_exists('$reproit', $value);
    check(
        'folding ' . $case['field'],
        $redacted === $case['secret'],
        $case['secret'] ? 'should be secret' : 'should not be secret'
    );
}

// Redaction, nesting
foreach ($vectors['redaction']['nestingCases'] as $case) {
    $actual = \ReproitBackend\redact($case['input']);
    check(
        'nesting ' . json_encode($case['input']),
        json_encode($actual) === json_encode($case['expect']),
        'got ' . json_encode($actual)
    );
}

// The trigger token vocabulary; iOS and RN both shipped user-action.
$token = $vectors['triggerTokens']['bySdkKind']['backend'];
check('trigger token allowed', in_array($token, $vectors['triggerTokens']['allowed'], true));
$source = (string) file_get_contents(__DIR__ . '/../capture.php');
check('capture.php emits the token', str_contains($source, $token));
foreach ($vectors['triggerTokens']['rejected'] as $bad) {
    check("capture.php does not emit {$bad}", !str_contains($source, "'{$bad}'"));
}

echo "behavior_vectors_test: {$passed} passed, {$failed} failed\n";
exit($failed === 0 ? 0 : 1);
