<?php

// Semantics parity tests against sdk/reproit-backend-rs/src/lib.rs, mirroring
// sdk/reproit-backend-node/test/trace.test.js. Run: php test/trace_test.php

declare(strict_types=1);

namespace ReproitBackend\Test;

use ReproitBackend\BackendTrace;

use function ReproitBackend\canonical_json;
use function ReproitBackend\http_input;
use function ReproitBackend\selection;
use function ReproitBackend\trace_context_from_headers;

use const ReproitBackend\MAX_EVENTS;
use const ReproitBackend\MAX_HEADER_BYTES;

require __DIR__ . '/../reproit.php';
require __DIR__ . '/support.php';

function context(array $overrides = []): array
{
    return array_merge([
        'traceId' => 'trace-a',
        'actor' => null,
        'actionIndex' => 0,
        'build' => null,
        'configContract' => null,
    ], $overrides);
}

function decode_header(string $header): array
{
    $raw = base64_decode(strtr($header, '-_', '+/') . str_repeat('=', -\strlen($header) % 4 & 3));
    return json_decode($raw, true);
}

// emits bounded correlated redacted events
$headers = [
    'x-reproit-trace' => 'trace-a',
    'x-reproit-actor' => 'alice',
    'x-reproit-action' => '7',
    'x-reproit-build' => 'build-a',
    'x-reproit-config-contract' => 'contract-a',
];
$parsed = trace_context_from_headers(fn (string $name) => $headers[$name] ?? null);
$trace = BackendTrace::begin($parsed, 'createProject', [
    'tenant' => 'org-1',
    'idempotencyKey' => 'retry-secret',
    'input' => ['name' => 'demo', 'password' => 'abcdefgh'],
    'selections' => [selection('project.id', 'projectId')],
]);
$trace->effect('write', ['resource' => 'projects', 'key' => '1', 'tenant' => 'org-1']);
$trace->finish([
    'id' => 1,
    'apiKey' => 'sk_live_secret',
    'publishable_key' => 'pk_live_secret',
    'private-key' => 'private-secret',
    'access key' => 'access-secret',
    'signingKey' => 'signing-secret',
    'monkey' => 'harmless',
], 201, true, true);
check(\strlen($trace->header()) < MAX_HEADER_BYTES, 'header stays under the byte cap');
$events = $trace->events();
check_same(7, $events[0]['actionIndex'], 'actionIndex parsed from header');
check_same('build-a', $events[0]['build'], 'build propagated');
check_same('contract-a', $events[0]['configContract'], 'configContract propagated');
check_same(8, $events[0]['input']['password']['$reproit']['length'], 'password stub length');
check($events[0]['idempotencyKey'] !== 'retry-secret', 'idempotency key never raw');
check(
    preg_match('/^sha256:[0-9a-f]{24}$/', $events[0]['idempotencyKey']) === 1,
    'idempotency key hashed',
);
foreach (['apiKey', 'publishable_key', 'private-key', 'access key', 'signingKey'] as $field) {
    check_same(true, $events[2]['output'][$field]['$reproit']['redacted'], "redacts $field");
}
check_same('harmless', $events[2]['output']['monkey'], 'non-secret output untouched');
check_same(true, $events[2]['effectsComplete'], 'effectsComplete recorded');

// stays inactive without a trace header
check_same(null, trace_context_from_headers(fn ($name) => null), 'inert without header');
check_same(
    null,
    trace_context_from_headers(fn ($name) => $name === 'x-reproit-trace' ? '  ' : null),
    'inert on blank trace id',
);

// header is unpadded base64url of the canonical event json
$trace = BackendTrace::begin(context(), 'op', ['input' => ['b' => 1, 'a' => 2]]);
$trace->finish(['ok' => true], 200, true, true);
$header = $trace->header();
check(preg_match('/[+\/=]/', $header) !== 1, 'header has no +, /, or padding');
$decoded = decode_header($header);
check_same(
    json_decode(canonical_json($trace->events()), true),
    $decoded,
    'header decodes to the events',
);
$raw = base64_decode(strtr($header, '-_', '+/') . str_repeat('=', -\strlen($header) % 4 & 3));
check(strpos($raw, '"a":2') < strpos($raw, '"b":1'), 'keys sorted (BTreeMap order)');

// rejects effects after return and a second return
$trace = BackendTrace::begin(context(), 'op', ['input' => null]);
$trace->finish(null, 200, true, false);
check_throws(fn () => $trace->effect('read', []), 'AlreadyFinished', 'no effects after return');
check_throws(fn () => $trace->finish(null, 200, true, false), 'AlreadyFinished', 'one return');

// header before finish is rejected, oversized header is rejected
$open = BackendTrace::begin(context(), 'op', ['input' => null]);
check_throws(fn () => $open->header(), 'AlreadyFinished', 'header before finish rejected');
$big = BackendTrace::begin(context(), 'op', ['input' => null]);
$big->finish(['blob' => str_repeat('x', MAX_HEADER_BYTES)], 200, true, true);
check_throws(fn () => $big->header(), 'HeaderTooLarge', 'oversized header rejected');

// event count is capped at 256
$trace = BackendTrace::begin(context(), 'op', ['input' => null]);
for ($i = 1; $i < MAX_EVENTS; $i++) {
    $trace->effect('emit', ['event' => 'tick']);
}
check_throws(fn () => $trace->effect('emit', []), 'TooManyEvents', 'effect past cap rejected');
check_throws(fn () => $trace->finish(null, 200, true, false), 'TooManyEvents', 'return past cap');

// typed effects only, bounded identifiers only
$trace = BackendTrace::begin(context(), 'op', ['input' => null]);
check_throws(fn () => $trace->effect('mutate', []), 'InvalidOperation', 'untyped effect rejected');
check_throws(
    fn () => BackendTrace::begin(context(), '', []),
    'InvalidOperation',
    'empty operation rejected',
);
check_throws(
    fn () => BackendTrace::begin(context(), str_repeat('x', 257), []),
    'InvalidOperation',
    'over-long operation rejected',
);

// effect detail keeps only before, after, payload after redaction
$trace = BackendTrace::begin(context(), 'op', ['input' => null]);
$trace->effect('write', [
    'resource' => 'users',
    'detail' => ['before' => ['email' => 'a@b.c'], 'after' => ['name' => 'z'], 'extra' => 'drop'],
]);
$effect = $trace->events()[1];
check_same(true, $effect['before']['email']['$reproit']['redacted'], 'detail before redacted');
check_same('z', $effect['after']['name'], 'detail after kept');
check(!\array_key_exists('extra', $effect), 'detail extra dropped');

// canonical http input lowercases headers and preserves repeated values
$input = http_input([
    'body' => ['name' => 'demo'],
    'path' => ['project' => 'p1'],
    'query' => ['tag' => ['a', 'b']],
    'headers' => ['X-Mode' => 'safe'],
]);
check_same('safe', $input->headers['x-mode'], 'headers lowercased');
check_same(['a', 'b'], $input->query['tag'], 'repeated query values preserved');
$empty = http_input(['path' => [], 'query' => [], 'headers' => []]);
check_same('{}', canonical_json($empty), 'empty input encodes as {}');

// selections validate their paths
check(selection('project.id', 'projectId') !== null, 'plain selection accepted');
check(selection('items[].id', 'rows[].id', 'Widget') !== null, 'array selection accepted');
check_same(null, selection('1bad', 'ok'), 'bad schema path rejected');
check_same(null, selection('ok', 'ok', 'Bad.Condition'), 'bad type condition rejected');

// canonical json byte parity with the Node SDK (skipped when node is absent)
$node = trim((string) shell_exec('command -v node 2>/dev/null'));
if ($node !== '') {
    $literal = '{"zeta":1,"alpha":{"y":[1,2,{"b":null,"a":true}],"x":"héllo ☃"},'
        . '"slash":"a/b","ctl":"line\nbreak\ttab","empty_obj":{},"empty_arr":[],'
        . '"num":-2,"big":4294967295,"frac":1.5,"quote":"say \"hi\""}';
    $script = 'const {canonicalJson} = require(process.argv[1]);'
        . 'let raw = "";process.stdin.on("data", (c) => raw += c);'
        . 'process.stdin.on("end", () => process.stdout.write(canonicalJson(JSON.parse(raw))));';
    $index = realpath(__DIR__ . '/../../reproit-backend-node/index.js');
    $spec = [0 => ['pipe', 'r'], 1 => ['pipe', 'w'], 2 => ['pipe', 'w']];
    $proc = proc_open([$node, '-e', $script, $index], $spec, $pipes);
    fwrite($pipes[0], $literal);
    fclose($pipes[0]);
    $expected = stream_get_contents($pipes[1]);
    proc_close($proc);
    check_same($expected, canonical_json(json_decode($literal)), 'byte parity with Node SDK');
} else {
    fwrite(STDOUT, "skip: node not available for the canonical json golden check\n");
}

report('trace_test');
