<?php

declare(strict_types=1);

/*
 * Shared behavioral conformance vectors (sdk/capture-behavior-v1.json).
 *
 * Eleven SDKs hand implement one contract, so a defect otherwise has to be
 * found eleven times. Four instances of one class landed in a single day, and
 * every group here was written against one of them.
 *
 * What each group pins, and the real defect behind it:
 *
 *   bounds.cases             the inline body budget is BYTES, not characters.
 *                            4096 euro signs are 12288 bytes; a runtime
 *                            measuring string length records that inline and
 *                            the capsule blows a budget replay trusts.
 *   headers.cases            names lowercase, and the 32 header cap is taken
 *                            over NAME SORTED order. Go capped a randomized
 *                            map in arrival order and recorded a different
 *                            subset every run, so replay was unrepeatable.
 *   redaction.typeCases      the placeholder carries type and length.
 *   redaction.foldingCases   which field names fold to secret.
 *   redaction.nestingCases   redaction reaches nested objects and arrays.
 *   redaction.structureCases redaction is structure preserving: no key
 *                            dropped, no array shortened, an explicit null
 *                            still a null value. An encoder that dropped null
 *                            map values changed the shape the replay matcher
 *                            walks, and replay reproduced a DIFFERENT error.
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

/** Key order is not part of the contract, so compare canonically. */
function sorted_keys(mixed $value): mixed
{
    if (!\is_array($value)) {
        return $value;
    }
    $value = array_map('sorted_keys', $value);
    if (!array_is_list($value)) {
        ksort($value);
    }
    return $value;
}

function same(mixed $actual, mixed $expect): bool
{
    return json_encode(sorted_keys($actual)) === json_encode(sorted_keys($expect));
}

// Bounds: `bodyRepeat` keeps the vectors small on disk.
foreach ($vectors['bounds']['cases'] as $case) {
    $body = isset($case['input']['bodyRepeat'])
        ? str_repeat($case['input']['bodyRepeat'][0], $case['input']['bodyRepeat'][1])
        : $case['input']['body'];
    $expect = $case['expect'];
    if (isset($expect['body']['repeat'])) {
        $expect['body'] = str_repeat($expect['body']['repeat'][0], $expect['body']['repeat'][1]);
    }
    $actual = \ReproitBackend\bounded_body($body, $case['input']['contentType']);
    check(
        'bounds ' . $case['name'],
        same($actual, $expect),
        'got ' . json_encode($actual)
    );
}

// Headers: literal cases, then the generated cap case fed in a deterministic
// NON-sorted order so a cap taken over arrival order keeps the wrong subset.
foreach ($vectors['headers']['cases'] as $case) {
    if (isset($case['input'])) {
        $actual = \ReproitBackend\bounded_headers($case['input']['headers']);
        check(
            'headers ' . $case['name'],
            same($actual, $case['expect']),
            'got ' . json_encode($actual)
        );
        continue;
    }
    $spec = $case['inputGenerated'];
    $count = $spec['headerCount'];
    $shuffled = [];
    for ($index = 0; $index < $count; $index++) {
        // 17 is coprime with 40, so this walks every name exactly once.
        $shuffled[sprintf($spec['namePattern'], ($index * 17) % $count)] = $spec['value'];
    }
    $kept = \ReproitBackend\bounded_headers($shuffled)['headers'] ?? [];
    $names = array_keys($kept);
    check(
        'headers ' . $case['name'] . ' count',
        \count($names) === $case['expect']['headerCount'],
        'kept ' . \count($names) . ' headers'
    );
    check(
        'headers ' . $case['name'] . ' first',
        ($names[0] ?? null) === $case['expect']['firstName'],
        'the cap must be taken over sorted names; first kept is ' . ($names[0] ?? 'none')
    );
    check(
        'headers ' . $case['name'] . ' last',
        (end($names) ?: null) === $case['expect']['lastName'],
        'the cap must be taken over sorted names; last kept is ' . (end($names) ?: 'none')
    );
}

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

// Redaction, structure preservation
foreach ($vectors['redaction']['structureCases'] as $case) {
    $actual = \ReproitBackend\redact($case['input']);
    check(
        'structure ' . $case['name'],
        same($actual, $case['expect']),
        'got ' . json_encode($actual) . ' want ' . json_encode($case['expect'])
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
