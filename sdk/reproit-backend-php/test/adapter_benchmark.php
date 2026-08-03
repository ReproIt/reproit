<?php
declare(strict_types=1);

require dirname(__DIR__) . '/trace.php';

use ReproitBackend\BackendTrace;

const DEPENDENCIES = 64;

function configured(string $name, int $fallback): int
{
    $raw = getenv($name);
    return $raw !== false && (int) $raw > 0 ? (int) $raw : $fallback;
}

function median(array $values): float
{
    sort($values, SORT_NUMERIC);
    return (float) $values[intdiv(count($values), 2)];
}

function free_port(): int
{
    $socket = stream_socket_server('tcp://127.0.0.1:0', $errno, $error);
    if ($socket === false) throw new RuntimeException($error, $errno);
    $address = stream_socket_get_name($socket, false);
    fclose($socket);
    return (int) substr(strrchr((string) $address, ':'), 1);
}

function http_cost(bool $mounted, bool $traced, int $runs): float
{
    $port = free_port();
    $fixture = dirname(__DIR__) . '/validation/adapter_benchmark_server.php';
    $command = [PHP_BINARY, '-S', "127.0.0.1:$port", $fixture];
    $environment = array_merge($_ENV, ['REPROIT_BENCH_MOUNTED' => $mounted ? '1' : '0']);
    $process = proc_open($command, [0 => ['pipe', 'r'], 1 => ['file', '/dev/null', 'a'],
        2 => ['file', '/dev/null', 'a']], $pipes, null, $environment);
    if (!is_resource($process)) throw new RuntimeException('php benchmark server did not start');
    $headers = $traced
        ? "x-reproit-trace: bench-trace\r\nConnection: close\r\n"
        : "Connection: close\r\n";
    $context = stream_context_create(['http' => ['header' => $headers, 'timeout' => 5]]);
    $fire = function () use ($port, $context): void {
        $body = @file_get_contents("http://127.0.0.1:$port/account?id=42", false, $context);
        if ($body === false) throw new RuntimeException('benchmark request failed');
    };
    try {
        for ($attempt = 0; $attempt < 100; $attempt++) {
            try { $fire(); break; } catch (Throwable) { usleep(10_000); }
        }
        for ($index = 0; $index < min(100, intdiv($runs, 4)); $index++) $fire();
        $started = hrtime(true);
        for ($index = 0; $index < $runs; $index++) $fire();
        return (hrtime(true) - $started) / 1000 / $runs;
    } finally {
        proc_terminate($process);
        proc_close($process);
    }
}

function dependency_cost(bool $captured, int $runs): float
{
    $context = ['traceId' => 'dependency-benchmark', 'actionIndex' => 1];
    $exchange = ['request' => ['method' => 'GET', 'url' => 'http://pricing.test/quote?tier=gold'],
        'response' => ['status' => 200, 'body' => ['price' => 42]]];
    $started = hrtime(true);
    for ($run = 0; $run < $runs; $run++) {
        $trace = BackendTrace::begin($context, 'dependencyBenchmark');
        if (!$captured) continue;
        for ($index = 0; $index < DEPENDENCIES; $index++) {
            $trace->effect('call', ['resource' => 'pricing', 'key' => (string) $index,
                'exchange' => $exchange]);
        }
    }
    return (hrtime(true) - $started) / 1000 / ($runs * DEPENDENCIES);
}

$runs = configured('REPROIT_ADAPTER_BENCH_RUNS', 500);
$rounds = configured('REPROIT_ADAPTER_BENCH_ROUNDS', 5);
$samples = ['baseline' => [], 'inactive' => [], 'active' => [], 'control' => []];
$dependencies = ['baseline' => [], 'captured' => [], 'control' => []];
for ($round = 0; $round < $rounds; $round++) {
    $samples['baseline'][] = http_cost(false, false, $runs);
    $samples['inactive'][] = http_cost(true, false, $runs);
    $samples['active'][] = http_cost(true, true, $runs);
    $samples['control'][] = http_cost(false, false, $runs);
    $dependencies['baseline'][] = dependency_cost(false, $runs);
    $dependencies['captured'][] = dependency_cost(true, $runs);
    $dependencies['control'][] = dependency_cost(false, $runs);
}
$baseline = median($samples['baseline']);
$noise = abs(median($samples['control']) - $baseline);
$inactive = median($samples['inactive']) - $baseline;
$active = median($samples['active']) - $baseline;
$depBaseline = median($dependencies['baseline']);
$depNoise = abs(median($dependencies['control']) - $depBaseline);
$depCost = median($dependencies['captured']) - $depBaseline;
if ($noise >= 2000 || $inactive >= 2000 || $active >= 5000 || $depNoise >= 20 || $depCost >= 100) {
    throw new RuntimeException(
        "PHP benchmark outside ceiling: noise=$noise inactive=$inactive "
        . "active=$active depNoise=$depNoise depCost=$depCost",
    );
}
echo json_encode(['language' => 'php', 'runs' => $runs, 'rounds' => $rounds,
    'noiseFloorMicros' => round($noise, 2), 'baselineMicros' => round($baseline, 2),
    'inactiveCostMicros' => round($inactive, 2), 'activeCostMicros' => round($active, 2),
    'dependencyNoiseFloorMicros' => round($depNoise, 2),
    'dependencyCaptureCostMicros' => round($depCost, 2), 'dependencyCeilingMicros' => 100],
    JSON_UNESCAPED_SLASHES) . "\n";
