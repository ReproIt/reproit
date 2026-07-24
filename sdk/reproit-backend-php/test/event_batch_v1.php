<?php

/*!
 * PHP mirror of sdk/test/event_batch_v1.js (itself a mirror of
 * reproit_protocol::EventBatch::validate), scoped to the event kinds the
 * production SDKs emit. Any batch this SDK builds must pass unchanged.
 * Throws RuntimeException('<reason-code>') on the first defect. JSON objects
 * are non-empty associative arrays or stdClass; lists are PHP list arrays.
 */

declare(strict_types=1);

namespace ReproitBackend\Test;

const MAX_BATCH_FRAMES = 5000;
const MAX_BATCH_GRAPHS = 256;
const MAX_FRAME_BYTES = 1024 * 1024;
const MAX_TOKEN_BYTES = 128;
const MAX_TEXT_BYTES = 16 * 1024;
const MAX_CONTEXT_BYTES = 64 * 1024;

function fail(string $reason): never
{
    throw new \RuntimeException($reason);
}

function is_obj(mixed $value): bool
{
    return $value instanceof \stdClass || (\is_array($value) && !array_is_list($value));
}

function obj_vars(mixed $value): array
{
    return $value instanceof \stdClass ? get_object_vars($value) : $value;
}

function only_keys(mixed $value, array $allowed, string $reason): void
{
    foreach (array_keys(obj_vars($value)) as $key) {
        if (!\in_array((string) $key, $allowed, true)) {
            fail($reason);
        }
    }
}

function field(mixed $value, string $key): mixed
{
    return obj_vars($value)[$key] ?? null;
}

function token(mixed $value): void
{
    if (
        !\is_string($value)
        || $value === ''
        || \strlen($value) > MAX_TOKEN_BYTES
        || preg_match('/^[A-Za-z0-9._:-]+$/', $value) !== 1
    ) {
        fail('invalid-event');
    }
}

function lower_token(mixed $value): void
{
    token($value);
    if (preg_match('/^[a-z0-9_-]+$/', $value) !== 1) {
        fail('invalid-event');
    }
}

function text(mixed $value, int $maxBytes): void
{
    if (!\is_string($value) || \strlen($value) > $maxBytes) {
        fail('invalid-event');
    }
}

function optional_text(mixed $value, int $maxBytes): void
{
    if ($value !== null) {
        text($value, $maxBytes);
    }
}

function value_bytes(mixed $value, int $maxBytes): void
{
    $encoded = json_encode($value, JSON_UNESCAPED_SLASHES | JSON_UNESCAPED_UNICODE);
    if ($encoded === false || \strlen($encoded) > $maxBytes) {
        fail('invalid-event');
    }
}

function validate_scope(mixed $scope): void
{
    if (!is_obj($scope)) {
        fail('invalid-scope');
    }
    $domain = field($scope, 'domain');
    if ($domain === 'shared' || $domain === 'backend') {
        only_keys($scope, ['domain'], 'invalid-scope');
        return;
    }
    if ($domain !== 'contract') {
        fail('invalid-scope');
    }
    only_keys($scope, ['domain', 'contractHash'], 'invalid-scope');
    $hash = field($scope, 'contractHash');
    if ($hash !== null && preg_match('/^[0-9a-f]{16}$/', (string) $hash) !== 1) {
        fail('invalid-scope');
    }
}

function validate_identity(mixed $identity): void
{
    if (!is_obj($identity)) {
        fail('invalid-event');
    }
    only_keys(
        $identity,
        ['oracle', 'invariant', 'kind', 'message', 'frame', 'trigger', 'boundary'],
        'invalid-event',
    );
    lower_token(field($identity, 'oracle'));
    foreach (['invariant', 'kind', 'message', 'frame', 'trigger'] as $name) {
        text(field($identity, $name), MAX_TEXT_BYTES);
    }
    optional_text(field($identity, 'boundary'), MAX_TEXT_BYTES);
}

function validate_event(mixed $event): void
{
    if (!is_obj($event)) {
        fail('invalid-event');
    }
    switch (field($event, 'kind')) {
        case 'backend':
            only_keys($event, ['kind', 'evidence'], 'invalid-event');
            value_bytes(field($event, 'evidence'), MAX_CONTEXT_BYTES);
            return;
        case 'graph-edge':
            only_keys($event, ['kind', 'from', 'action', 'to'], 'invalid-event');
            text(field($event, 'from'), MAX_TEXT_BYTES);
            text(field($event, 'action'), MAX_TEXT_BYTES);
            text(field($event, 'to'), MAX_TEXT_BYTES);
            return;
        case 'finding':
            only_keys(
                $event,
                ['kind', 'signature', 'message', 'identity', 'path', 'context'],
                'invalid-event',
            );
            text(field($event, 'signature'), MAX_TEXT_BYTES);
            text(field($event, 'message'), MAX_TEXT_BYTES);
            validate_identity(field($event, 'identity'));
            $path = field($event, 'path');
            if (!\is_array($path) || !array_is_list($path) || \count($path) > 256) {
                fail('invalid-event');
            }
            foreach ($path as $step) {
                if (!is_obj($step)) {
                    fail('invalid-event');
                }
                only_keys($step, ['signature', 'action', 'label'], 'invalid-event');
                text(field($step, 'signature'), MAX_TEXT_BYTES);
                text(field($step, 'action'), MAX_TEXT_BYTES);
                optional_text(field($step, 'label'), MAX_TEXT_BYTES);
            }
            if (!is_obj(field($event, 'context'))) {
                fail('invalid-event');
            }
            value_bytes(field($event, 'context'), MAX_CONTEXT_BYTES);
            return;
        default:
            fail('invalid-event');
    }
}

function validate_frame(mixed $frame): void
{
    if (!is_obj($frame)) {
        fail('malformed-frame');
    }
    only_keys($frame, ['runId', 'sequence', 'scope', 'event'], 'malformed-frame');
    token(field($frame, 'runId'));
    $sequence = field($frame, 'sequence');
    if (!\is_int($sequence) || $sequence < 0) {
        fail('invalid-sequence');
    }
    validate_scope(field($frame, 'scope'));
    validate_event(field($frame, 'event'));
    value_bytes(field($frame, 'event'), MAX_FRAME_BYTES);
}

function validate_event_batch(mixed $batch): void
{
    if (!is_obj($batch)) {
        fail('malformed-frame');
    }
    only_keys(
        $batch,
        ['version', 'batchId', 'appId', 'deployment', 'frames', 'evidence'],
        'invalid-event',
    );
    if (field($batch, 'version') !== 1) {
        fail('unsupported-version');
    }
    token(field($batch, 'batchId'));
    token(field($batch, 'appId'));
    $deployment = field($batch, 'deployment');
    if ($deployment !== null) {
        if (!is_obj($deployment)) {
            fail('invalid-event');
        }
        only_keys($deployment, ['version', 'commit'], 'invalid-event');
        $version = field($deployment, 'version');
        $commit = field($deployment, 'commit');
        if ($version === null && $commit === null) {
            fail('invalid-event');
        }
        if ($version !== null) {
            token($version);
        }
        if ($commit !== null) {
            token($commit);
        }
    }
    $frames = field($batch, 'frames');
    $evidence = field($batch, 'evidence');
    $framesIsList = \is_array($frames) && array_is_list($frames);
    $evidenceIsList = \is_array($evidence) && array_is_list($evidence);
    if (!$framesIsList || !$evidenceIsList) {
        fail('invalid-event');
    }
    if (\count($frames) > MAX_BATCH_FRAMES || \count($evidence) > MAX_BATCH_GRAPHS) {
        fail('batch-too-large');
    }
    if ($frames === [] && $evidence === []) {
        fail('invalid-event');
    }
    $lastSequence = null;
    foreach ($frames as $frame) {
        validate_frame($frame);
        $sequence = field($frame, 'sequence');
        if ($lastSequence !== null && $sequence <= $lastSequence) {
            fail('invalid-sequence');
        }
        $lastSequence = $sequence;
    }
}
