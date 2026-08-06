<?php

/*!
 * auto_prepend_file bootstrap for reproit-backend-php.
 *
 * Set this file as PHP's `auto_prepend_file` and every request loads the SDK
 * and turns on automatic outbound-stream capture before the application runs:
 *
 *   php -d auto_prepend_file=/path/to/bootstrap.php app.php
 *   ; or in php.ini / a pool config:
 *   auto_prepend_file = /path/to/reproit-backend-php/bootstrap.php
 *
 * What this makes AUTOMATIC: http:// and https:// STREAM traffic. Any
 * file_get_contents, fopen, SimpleXML, or DOMDocument::load on an http(s) URL
 * is captured with no per-call change, through the same recording path as
 * `Instrument::http` (the wrapper delegates to it). In replay mode
 * (`REPROIT_REPLAY` set) the wrapper serves the recorded exchange with no
 * socket and fails closed on divergence.
 *
 * What stays OPT-IN, and why: `curl_exec` and PDO. curl and the PDO drivers
 * are C-level functions. PHP cannot redefine or intercept a C function at
 * runtime without the uopz or runkit extension, and neither is present (nor
 * an acceptable production dependency). Route curl-direct calls through
 * `Instrument::http` and database statements through `RecordingPdo`, or they
 * are outside the capsule. See README.md.
 *
 * Installing the wrapper is the only side effect. It does not begin a trace;
 * the framework adapter (psr15.php / vanilla.php) still sets the ambient
 * trace, and capture records onto it only when one is present.
 */

declare(strict_types=1);

require_once __DIR__ . '/reproit.php';

\ReproitBackend\install();
