<?php

/*!
 * Hermetic replay mode for reproit-backend-php.
 *
 * When `REPROIT_REPLAY` names a `reproit-backend-capture` payload, the same
 * outbound client decorator and database helper that record exchanges at
 * capture time SERVE them instead, so the application re-executes against
 * exactly what production saw with no live dependency.
 *
 * Determinism is a contract here, not a similarity score. Matching is
 * strict: the next unconsumed exchange of the same protocol, same method and
 * path, body modulo `$reproit` redaction placeholders. The first unmatched
 * call is a DIVERGENCE: it writes the structured `REPROIT:DIVERGENCE ` line
 * to stderr and the call fails with status 599 (HTTP) or a thrown error
 * (database), never a fuzzy match.
 *
 * The envelope pins replay determinism: the capture's timezone is applied
 * and `replay_rng` yields the seeded stream. Honesty note: the seed makes
 * REPLAY runs deterministic; it does not reproduce the randomness the app
 * drew in production.
 */

declare(strict_types=1);

namespace ReproitBackend;

require_once __DIR__ . '/exchange.php';

const DIVERGENCE_MARKER = 'REPROIT:DIVERGENCE ';

/**
 * Wire-parity JSON: compact, insertion order preserved, slashes and unicode
 * unescaped, exactly the bytes Node's JSON.stringify emits for the same
 * parse-ordered value. The divergence marker and replayed JSON bodies use
 * THIS encoding, never canonical_json: canonical sorting would re-order keys
 * the Node reference leaves in place and break the byte-compare parity pin.
 */
function marker_json(mixed $value): string
{
    $encoded = json_encode($value, JSON_UNESCAPED_SLASHES | JSON_UNESCAPED_UNICODE);
    return $encoded === false ? 'null' : $encoded;
}

/**
 * Sentinel for "the request carries no body field at all", distinct from a
 * recorded JSON null (which is a value the matcher wildcards on). PHP's `??`
 * cannot tell the two apart, so presence is probed with array_key_exists and
 * absence is carried as this singleton.
 */
final class Absent
{
    private static ?self $instance = null;

    public static function value(): self
    {
        return self::$instance ??= new self();
    }
}

/** The body of a request array, or the Absent sentinel when the key is gone. */
function body_or_absent(?array $request): mixed
{
    if ($request === null || !\array_key_exists('body', $request)) {
        return Absent::value();
    }
    return $request['body'];
}

/**
 * One operation's identity for ordinal matching: HTTP is method plus path
 * and query, database is the exact statement text.
 */
function operation_key(string $protocol, array $request): string
{
    if ($protocol === 'http') {
        return (string) ($request['method'] ?? '') . ' '
            . path_and_query((string) ($request['url'] ?? ''));
    }
    return (string) ($request['text'] ?? '');
}

/** The messages array of an OpenAI/Anthropic-shaped chat body, else null. */
function chat_messages(mixed $body): ?array
{
    $messages = \is_array($body) ? ($body['messages'] ?? null) : null;
    return \is_array($messages) && array_is_list($messages) ? $messages : null;
}

function delta_bytes(mixed $value): string
{
    return \is_string($value) ? $value : marker_json($value);
}

/**
 * Locate the first difference between a recorded request body and a live
 * one, modulo redaction placeholders. Null when there is nothing to report
 * (either body absent, or no difference the matcher would object to).
 * Chat-shaped bodies name the first differing message index (prompt drift);
 * unknown shapes fall back to the byte offset of the first differing byte.
 */
function body_delta(mixed $recorded, mixed $live): ?array
{
    if ($recorded instanceof Absent || $live instanceof Absent) {
        return null;
    }
    if (replay_matches($recorded, $live)) {
        return null;
    }
    $recordedMessages = chat_messages($recorded);
    $liveMessages = chat_messages($live);
    if ($recordedMessages !== null && $liveMessages !== null) {
        $bound = min(\count($recordedMessages), \count($liveMessages));
        $index = null;
        for ($i = 0; $i < $bound; $i++) {
            if (!replay_matches($recordedMessages[$i], $liveMessages[$i])) {
                $index = $i;
                break;
            }
        }
        // All shared indexes match: the drift is a longer or shorter
        // conversation, and the first differing message is the first
        // unshared one. If lengths also agree the drift is outside
        // `messages`; fall through to bytes.
        if ($index === null && \count($recordedMessages) !== \count($liveMessages)) {
            $index = $bound;
        }
        if ($index !== null) {
            return [
                'kind' => 'message',
                'firstDifferingMessage' => $index,
                'recordedMessages' => \count($recordedMessages),
                'liveMessages' => \count($liveMessages),
            ];
        }
    }
    $recordedBytes = delta_bytes($recorded);
    $liveBytes = delta_bytes($live);
    $bound = min(\strlen($recordedBytes), \strlen($liveBytes));
    $offset = $bound;
    for ($i = 0; $i < $bound; $i++) {
        if ($recordedBytes[$i] !== $liveBytes[$i]) {
            $offset = $i;
            break;
        }
    }
    return ['kind' => 'byte', 'offset' => $offset];
}

/** Deterministic xorshift64* stream, identical to the other SDKs. */
final class ReplayRng
{
    private int $state;

    public function __construct(int $seed)
    {
        $this->state = $seed | 1;
    }

    /** The next draw in [0, 1). */
    public function nextFloat(): float
    {
        $this->state ^= ($this->state << 13);
        $this->state ^= ($this->state >> 7) & 0x01FFFFFFFFFFFFFF;
        $this->state ^= ($this->state << 17);
        $mixed = self::mul64($this->state, 0x2545F4914F6CDD1D);
        return (($mixed >> 11) & 0x001FFFFFFFFFFFFF) / (float) (1 << 53);
    }

    /**
     * Wrapping 64-bit multiply. PHP has no unsigned 64-bit integer and a
     * plain `*` overflows into a float (losing the low bits the stream
     * depends on), so this multiplies in 16-bit limbs and discards above
     * bit 64, matching the other SDKs' wrapping semantics exactly.
     */
    private static function mul64(int $a, int $b): int
    {
        $left = [$a & 0xFFFF, ($a >> 16) & 0xFFFF, ($a >> 32) & 0xFFFF, ($a >> 48) & 0xFFFF];
        $right = [$b & 0xFFFF, ($b >> 16) & 0xFFFF, ($b >> 32) & 0xFFFF, ($b >> 48) & 0xFFFF];
        $limbs = [0, 0, 0, 0];
        for ($i = 0; $i < 4; $i++) {
            for ($j = 0; $i + $j < 4; $j++) {
                $limbs[$i + $j] += $left[$i] * $right[$j];
            }
        }
        $carry = 0;
        for ($k = 0; $k < 4; $k++) {
            $value = $limbs[$k] + $carry;
            $limbs[$k] = $value & 0xFFFF;
            $carry = $value >> 16;
        }
        return $limbs[0] | ($limbs[1] << 16) | ($limbs[2] << 32) | ($limbs[3] << 48);
    }
}

final class ReplaySession
{
    /** @var list<array{exchange: array, consumed: bool}> */
    private array $entries = [];
    private ?array $envelope;
    private bool $diverged = false;

    public static function load(string $path): self
    {
        $raw = @file_get_contents($path);
        if ($raw === false) {
            throw new \RuntimeException('REPROIT_REPLAY file is unreadable: ' . $path);
        }
        $payload = json_decode($raw, true);
        if (!\is_array($payload) || ($payload['format'] ?? null) !== 'reproit-backend-capture') {
            throw new \RuntimeException(
                'REPROIT_REPLAY file is not a reproit-backend-capture payload'
            );
        }
        $version = $payload['version'] ?? null;
        if (!\is_int($version) || $version < 1 || $version > 2) {
            throw new \RuntimeException('unsupported capture version');
        }
        return new self($payload);
    }

    private function __construct(array $payload)
    {
        $this->envelope = \is_array($payload['envelope'] ?? null) ? $payload['envelope'] : null;
        foreach ($payload['events'] ?? [] as $event) {
            if (!\is_array($event) || ($event['kind'] ?? null) !== 'effect') {
                continue;
            }
            if (!\is_array($event['exchange'] ?? null)) {
                continue;
            }
            $this->entries[] = ['exchange' => $event['exchange'], 'consumed' => false];
        }
    }

    public function envelope(): ?array
    {
        return $this->envelope;
    }

    public function diverged(): bool
    {
        return $this->diverged;
    }

    /**
     * Strict per-operation ordinal match, the Node reference policy: within
     * one operation (method plus path for HTTP, statement text for the
     * database) exchanges are consumed in recorded order, so pooled database
     * clients and tool-call loops that interleave operations still match
     * exactly. Returns the exchange or null (divergence).
     */
    public function match(string $protocol, array $probe): ?array
    {
        $key = operation_key($protocol, $probe);
        foreach ($this->entries as $index => $entry) {
            if ($entry['consumed'] || ($entry['exchange']['protocol'] ?? null) !== $protocol) {
                continue;
            }
            if (operation_key($protocol, $entry['exchange']['request'] ?? []) !== $key) {
                continue;
            }
            if (request_matches($protocol, $entry['exchange']['request'] ?? [], $probe)) {
                $this->entries[$index]['consumed'] = true;
                return $entry['exchange'];
            }
            // Strict ordinal within an operation: the next unconsumed
            // exchange of THIS operation is the only candidate; skipping it
            // silently would be a fuzzy match. Other operations' exchanges
            // may interleave, which is why the key filters above.
            break;
        }
        $this->diverge($protocol, $probe);
        return null;
    }

    public function diverge(string $protocol, array $probe): void
    {
        $this->diverged = true;
        $key = operation_key($protocol, $probe);
        $consumed = 0;
        $sameKey = null;
        $sameProtocol = null;
        foreach ($this->entries as $entry) {
            if ($entry['consumed']) {
                $consumed++;
                continue;
            }
            if (($entry['exchange']['protocol'] ?? null) !== $protocol) {
                continue;
            }
            $request = $entry['exchange']['request'] ?? [];
            $sameProtocol ??= $request;
            if ($sameKey === null && operation_key($protocol, $request) === $key) {
                $sameKey = $request;
            }
        }
        $expected = $sameKey ?? $sameProtocol;
        // Field order mirrors the Node reference so the marker line is
        // byte-comparable across SDKs; marker_json for the same reason.
        $report = [
            'protocol' => $protocol,
            'got' => $probe,
            'expected' => $expected,
            'consumed' => $consumed,
            'total' => \count($this->entries),
        ];
        // Prompt drift: when the recorded and live bodies both exist and
        // differ, name WHERE they differ (see body_delta).
        $delta = $expected === null
            ? null
            : body_delta(body_or_absent($expected), body_or_absent($probe));
        if ($delta !== null) {
            $report['bodyDelta'] = $delta;
        }
        // Written raw to stderr: the line must be byte-identical to the
        // Node, Rust, and Ruby SDKs' so one CLI parser reads every platform.
        $handle = fopen('php://stderr', 'w');
        if ($handle !== false) {
            fwrite($handle, DIVERGENCE_MARKER . marker_json($report) . "\n");
            fclose($handle);
        }
    }
}

/**
 * A recorded value matches a live one when equal, when the recorded side is
 * a `$reproit` redaction placeholder (any value stood there at capture), or
 * when the recorded side is absent. Arrays compare per key.
 */
function replay_matches(mixed $recorded, mixed $live): bool
{
    if ($recorded === null) {
        return true;
    }
    if (\is_array($recorded)) {
        if (\array_key_exists('$reproit', $recorded)) {
            return true;
        }
        if (!\is_array($live)) {
            return false;
        }
        if (array_is_list($recorded) !== array_is_list($live)) {
            return false;
        }
        if (array_is_list($recorded) && \count($recorded) !== \count($live)) {
            return false;
        }
        foreach ($recorded as $key => $value) {
            if (!replay_matches($value, $live[$key] ?? null)) {
                return false;
            }
        }
        return true;
    }
    return $recorded === $live;
}

function request_matches(string $protocol, array $recorded, array $probe): bool
{
    if ($protocol === 'http') {
        if (($recorded['method'] ?? null) !== ($probe['method'] ?? null)) {
            return false;
        }
        if (path_and_query((string) ($recorded['url'] ?? '')) !==
            path_and_query((string) ($probe['url'] ?? ''))) {
            return false;
        }
        // Recorded headers are deliberately not matched: they carry per-run
        // noise that would turn every replay into a divergence.
        return replay_matches($recorded['body'] ?? null, $probe['body'] ?? null);
    }
    if (($recorded['text'] ?? null) !== ($probe['text'] ?? null)) {
        return false;
    }
    return replay_matches($recorded['values'] ?? null, $probe['values'] ?? null);
}

function path_and_query(string $url): string
{
    $parts = parse_url($url);
    if ($parts === false) {
        return $url;
    }
    $path = $parts['path'] ?? '/';
    if ($path === '') {
        $path = '/';
    }
    return isset($parts['query']) ? $path . '?' . $parts['query'] : $path;
}

/**
 * Resolve a live HTTP probe entirely in process. A divergence and a
 * truncated-at-capture body both serve a hard 599 so the application
 * observes an attributable failure instead of a guess.
 *
 * @return array{status: int, headers: array<string, string>, body: string}
 */
function serve_http(ReplaySession $session, array $probe): array
{
    $recorded = $session->match('http', $probe);
    if ($recorded === null) {
        return diverged_599('diverged');
    }
    $response = $recorded['response'] ?? [];
    if (($response['truncated'] ?? false) === true) {
        // The capture kept identity but not bytes; serving a guessed body
        // would be a silent lie. Fail closed with the named reason.
        $session->diverge('http', $probe + ['truncated' => true]);
        return diverged_599('truncated-exchange-body');
    }
    $headers = [];
    foreach ($response['headers'] ?? [] as $name => $value) {
        $lower = strtolower((string) $name);
        if (\in_array($lower, ['content-length', 'transfer-encoding', 'content-encoding'], true)) {
            continue;
        }
        $headers[$lower] = (string) $value;
    }
    $body = $response['body'] ?? null;
    $text = match (true) {
        $body === null => '',
        \is_string($body) => $body,
        // marker_json, not canonical_json: byte-identical to the Node
        // reference's JSON.stringify of the same recorded body.
        default => marker_json($body),
    };
    $served = [
        'status' => (int) ($response['status'] ?? 200),
        'headers' => $headers,
        'body' => $text,
    ];
    $stream = $response['stream'] ?? null;
    if (\is_array($stream) && \is_array($stream['chunks'] ?? null)) {
        if (($stream['truncated'] ?? false) === true) {
            // The capture kept the body but not every chunk boundary;
            // serving a guessed stream shape would be a silent lie. Fail
            // closed with the named reason.
            $session->diverge('http', $probe + ['streamBoundariesTruncated' => true]);
            return diverged_599('truncated-stream-boundaries');
        }
        $served['chunks'] = split_chunks($text, $stream['chunks']);
    }
    return $served;
}

/**
 * Split a replayed body at the recorded chunk boundaries (byte lengths).
 * Redaction can change body byte counts, so lengths are clamped and the last
 * chunk absorbs any remainder: the CHUNK COUNT (the stream shape the app
 * observed) is preserved exactly, the recorded content never padded.
 *
 * @return list<string>
 */
function split_chunks(string $body, array $lengths): array
{
    $chunks = [];
    $offset = 0;
    $count = \count($lengths);
    foreach (array_values($lengths) as $index => $length) {
        $last = $index === $count - 1;
        $size = \is_int($length) && $length > 0 ? $length : 0;
        $end = $last ? \strlen($body) : min($offset + $size, \strlen($body));
        $chunks[] = substr($body, $offset, $end - $offset);
        $offset = $end;
    }
    return $chunks;
}

function diverged_599(string $reason): array
{
    return [
        'status' => 599,
        'headers' => ['content-type' => 'application/json'],
        'body' => canonical_json(['reproit' => $reason]),
    ];
}

function try_json(string $text, string $contentType): mixed
{
    if (!str_contains($contentType, 'application/json')) {
        return $text;
    }
    $decoded = json_decode($text, true);
    return json_last_error() === JSON_ERROR_NONE ? $decoded : $text;
}

/**
 * Pin process determinism from the capture envelope: the timezone and the
 * seeded stream. The wall CLOCK is deliberately not pinned, and this is a
 * hard platform limit rather than a deferred task. Two facts, both measured:
 *
 *  1. Redeclaring an internal function in the global scope is a fatal error
 *     ("Cannot redeclare function time()"), so `time()` and `microtime()`
 *     cannot be replaced process wide.
 *  2. PHP's namespaced-function fallback shadows only UNQUALIFIED calls made
 *     from inside the same namespace. A probe defining `Reproit\time()`
 *     shadowed calls within `Reproit\` and left the application's own
 *     namespace resolving the REAL `time()`, so the shadow cannot reach the
 *     code being replayed.
 *
 * Intercepting the clock therefore needs an extension (uopz or runkit7),
 * which are development tools and not an acceptable dependency for an SDK
 * that loads in production. An application that needs an anchored clock in
 * replay should read `Instrument::replayObservedAtMs()` and use it as its
 * own time source, the same shape the .NET SDK uses on Windows.
 */
function pin_envelope(?array $envelope): void
{
    if ($envelope === null) {
        return;
    }
    $tz = $envelope['tz'] ?? null;
    if (\is_string($tz) && $tz !== '' && in_array($tz, timezone_identifiers_list(), true)) {
        date_default_timezone_set($tz);
    }
    // Seed the process Mersenne stream (mt_rand/mt_srand, and rand which
    // aliases it since PHP 7.1) from the capture's replaySeed, so replayed
    // code drawing from mt_rand is deterministic run to run. Named gap, not
    // pinnable: random_bytes/random_int are CSPRNG by design and accept no
    // seed, so code drawing from them stays nondeterministic in replay.
    $seed = $envelope['replaySeed'] ?? null;
    if (\is_string($seed) && $seed !== '') {
        mt_srand((int) hexdec(substr($seed, 0, 8)));
    }
}

/**
 * The capture's wall-clock instant in epoch milliseconds, or null when the
 * envelope carries none. Exposed because the clock cannot be pinned (see
 * pin_envelope): an app that must anchor time in replay reads this.
 */
function observed_at_ms(?array $envelope): ?int
{
    $observed = $envelope['observedAtMs'] ?? null;
    return \is_int($observed) || \is_float($observed) ? (int) $observed : null;
}

function rng_for(?array $envelope): ?ReplayRng
{
    $seed = $envelope['replaySeed'] ?? null;
    if (!\is_string($seed) || $seed === '') {
        return null;
    }
    return new ReplayRng((int) hexdec(str_pad(substr($seed, 0, 16), 16, '0')));
}

/**
 * The SDK-owned clock seam. `time()`/`microtime()` cannot be intercepted
 * without an extension (see pin_envelope for the measured evidence), so the
 * envelope's clock pin is expressed as an interface the application reads
 * instead of the ambient clock: in replay mode `Instrument::clock()` returns
 * a PinnedClock offset to the capture instant; everywhere else it returns
 * the system clock, so app code needs no mode branch of its own.
 */
interface Clock
{
    /** Wall-clock now in epoch milliseconds. */
    public function nowMs(): int;
}

final class SystemClock implements Clock
{
    public function nowMs(): int
    {
        return (int) (microtime(true) * 1000);
    }
}

final class PinnedClock implements Clock
{
    private int $offsetMs;

    public function __construct(int $observedAtMs)
    {
        $this->offsetMs = $observedAtMs - (int) (microtime(true) * 1000);
    }

    public function nowMs(): int
    {
        return (int) (microtime(true) * 1000) + $this->offsetMs;
    }
}
