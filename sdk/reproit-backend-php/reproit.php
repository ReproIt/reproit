<?php

/*!
 * Entry point for reproit-backend-php: loads the trace core and the capture
 * mode. The framework adapters (psr15.php, vanilla.php) are loaded separately
 * so hosts only pay for the integration they use.
 */

declare(strict_types=1);

require_once __DIR__ . '/trace.php';
require_once __DIR__ . '/capture.php';
