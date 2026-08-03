<?php

// Order pricing for the PHP flaky-CI fixture. The planted bug: an
// unrecognized config shape silently means "no tax", so when the legacy
// matrix has switched the config service to its wrapped legacy format the
// total is computed without tax. FIXED=1 selects the corrected reader that
// understands the legacy shape too.

declare(strict_types=1);

namespace PhpFlakyCiFixture;

use ReproitBackend\Instrument;

function order_total(int $amount, string $configUrl): float
{
    $response = Instrument::http('GET', $configUrl . '/tax-rate');
    $config = json_decode($response->body, true);
    $config = \is_array($config) ? $config : [];
    $rate = $config['rate'] ?? null;
    if (getenv('FIXED') === '1' && !\is_float($rate) && !\is_int($rate)) {
        // The fix: the legacy matrix answers the wrapped shape.
        $rate = $config['value']['rate'] ?? null;
    }
    if (!\is_float($rate) && !\is_int($rate)) {
        $rate = 0; // BUG: an unrecognized config shape silently means no tax
    }
    return (float) ($amount + $amount * $rate);
}
