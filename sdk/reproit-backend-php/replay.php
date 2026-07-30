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

    /** Strict next-unconsumed match. Returns the exchange or null. */
    public function match(string $protocol, array $probe): ?array
    {
        foreach ($this->entries as $index => $entry) {
            if ($entry['consumed'] || ($entry['exchange']['protocol'] ?? null) !== $protocol) {
                continue;
            }
            // Strict ordering within a protocol: the first unconsumed
            // exchange is the only candidate; skipping it would be a fuzzy
            // match.
            if (request_matches($protocol, $entry['exchange']['request'] ?? [], $probe)) {
                $this->entries[$index]['consumed'] = true;
                return $entry['exchange'];
            }
            break;
        }
        $this->diverge($protocol, $probe);
        return null;
    }

    public function diverge(string $protocol, array $probe): void
    {
        $this->diverged = true;
        $expected = null;
        $consumed = 0;
        foreach ($this->entries as $entry) {
            if ($entry['consumed']) {
                $consumed++;
                continue;
            }
            if ($expected === null && ($entry['exchange']['protocol'] ?? null) === $protocol) {
                $expected = $entry['exchange']['request'] ?? null;
            }
        }
        $report = [
            'protocol' => $protocol,
            'got' => $probe,
            'expected' => $expected,
            'consumed' => $consumed,
            'total' => \count($this->entries),
        ];
        // Written raw to stderr: the line must be byte-identical to the
        // Node, Rust, and Ruby SDKs' so one CLI parser reads every platform.
        $handle = fopen('php://stderr', 'w');
        if ($handle !== false) {
            fwrite($handle, DIVERGENCE_MARKER . canonical_json($report) . "\n");
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
        default => canonical_json($body),
    };
    return ['status' => (int) ($response['status'] ?? 200), 'headers' => $headers, 'body' => $text];
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
 * Pin process determinism from the capture envelope. PHP has no safe
 * process-wide clock override, so the clock is deliberately NOT pinned; the
 * timezone and the seeded stream are.
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
}

function rng_for(?array $envelope): ?ReplayRng
{
    $seed = $envelope['replaySeed'] ?? null;
    if (!\is_string($seed) || $seed === '') {
        return null;
    }
    return new ReplayRng((int) hexdec(str_pad(substr($seed, 0, 16), 16, '0')));
}
