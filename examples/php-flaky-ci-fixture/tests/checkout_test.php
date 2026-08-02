<?php

// Planted order-dependent test failure that fires only under CI-like
// conditions, for the flaky-CI wedge (Track 3), PHP edition.
//
// The first test runs ONLY on the CI legacy matrix (CI_LEGACY_MATRIX=1) and
// leaks state into the shared config service: it switches the service to
// its legacy response format, which wraps the tax rate. The second test
// then computes a wrong total and fails. A plain local run never takes the
// legacy branch, so the suite passes and the failure looks unreproducible
// ("flaky"). The capsule spooled by the CI run carries the recorded legacy
// response, so `reproit check <capsule> --exec "php tests/checkout_test.php"`
// re-executes the exact failing run anywhere.
//
// Run it directly (`php tests/checkout_test.php`): the SDK's plain-script
// CI wrapper (Ci::suite) owns the exit code and the stderr markers
// `reproit check` parses.

declare(strict_types=1);

require __DIR__ . '/../../../sdk/reproit-backend-php/reproit.php';
require __DIR__ . '/../../../sdk/reproit-backend-php/ci.php';
require __DIR__ . '/../order.php';

use ReproitBackend\Ci;
use ReproitBackend\Instrument;

use function PhpFlakyCiFixture\order_total;

const PORT = 19991;
$configUrl = 'http://127.0.0.1:' . PORT;

// The shared config service both tests talk to. Never started under replay,
// where the SDK serves the recorded exchanges in process and any real
// socket attempt would surface as a divergence, not a connection.
$replay = getenv('REPROIT_REPLAY');
if (!\is_string($replay) || $replay === '') {
    $state = sys_get_temp_dir() . '/reproit-php-flaky-' . getmypid();
    @mkdir($state, 0o777, true);
    $spec = [1 => ['file', '/dev/null', 'w'], 2 => ['file', '/dev/null', 'w']];
    $service = proc_open(
        [PHP_BINARY, '-S', '127.0.0.1:' . PORT, __DIR__ . '/../config_service.php'],
        $spec,
        $pipes,
        null,
        ['REPROIT_FIXTURE_STATE' => $state] + getenv()
    );
    if ($service === false) {
        fwrite(STDERR, "config service failed to start\n");
        exit(2);
    }
    $ready = false;
    for ($i = 0; $i < 100 && !$ready; $i++) {
        // Raw readiness probe, deliberately outside the SDK boundary: no
        // ambient trace exists yet, so nothing is recorded.
        $ready = @file_get_contents($configUrl . '/tax-rate') !== false;
        if (!$ready) {
            usleep(50000);
        }
    }
    register_shutdown_function(static function () use ($service, $state): void {
        @proc_terminate($service);
        @unlink($state . '/legacy.flag');
        @rmdir($state);
    });
    if (!$ready) {
        fwrite(STDERR, "config service never answered\n");
        exit(2);
    }
}

$test = Ci::suite('checkout');

$test('legacy config format toggles', function () use ($configUrl): void {
    // CI-only: this is the state leak that makes the next test order
    // dependent. A local run never takes this branch.
    if (getenv('CI_LEGACY_MATRIX') !== '1') {
        return;
    }
    $response = Instrument::http('POST', $configUrl . '/format/legacy');
    if ($response->status !== 204) {
        throw new \RuntimeException('legacy toggle answered ' . $response->status);
    }
});

$test('order total applies the configured tax rate', function () use ($configUrl): void {
    $total = order_total(100, $configUrl);
    if ($total !== 125.0) {
        throw new \RuntimeException('order total ' . $total . ' does not equal 125');
    }
});
