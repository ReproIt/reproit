/*
 * Java port of sdk/test/event_batch_v1.js: mirror of
 * `reproit_protocol::EventBatch::validate`, scoped to the event kinds the
 * production SDKs emit. Any batch this SDK builds must pass unchanged.
 * Throws IllegalStateException("<reason-code>") on the first defect.
 */
package dev.reproit.backend;

import java.nio.charset.StandardCharsets;
import java.util.List;
import java.util.Map;
import java.util.regex.Pattern;

final class EventBatchV1 {
    static final int MAX_BATCH_FRAMES = 5000;
    static final int MAX_BATCH_GRAPHS = 256;
    static final int MAX_FRAME_BYTES = 1024 * 1024;
    static final int MAX_TOKEN_BYTES = 128;
    static final int MAX_TEXT_BYTES = 16 * 1024;
    static final int MAX_CONTEXT_BYTES = 64 * 1024;

    private static final Pattern TOKEN = Pattern.compile("^[A-Za-z0-9._:-]+$");
    private static final Pattern LOWER_TOKEN = Pattern.compile("^[a-z0-9_-]+$");
    private static final Pattern CONTRACT_HASH = Pattern.compile("^[0-9a-f]{16}$");

    private EventBatchV1() {}

    private static void fail(String reason) {
        throw new IllegalStateException(reason);
    }

    private static Map<?, ?> asObject(Object value, String reason) {
        if (!(value instanceof Map<?, ?> map)) {
            fail(reason);
            throw new IllegalStateException("unreachable");
        }
        return map;
    }

    private static void onlyKeys(Map<?, ?> value, List<String> allowed, String reason) {
        for (Object key : value.keySet()) {
            if (!allowed.contains(String.valueOf(key))) fail(reason);
        }
    }

    private static void token(Object value) {
        if (!(value instanceof String text) || text.isEmpty()
                || text.getBytes(StandardCharsets.UTF_8).length > MAX_TOKEN_BYTES
                || !TOKEN.matcher(text).matches()) {
            fail("invalid-event");
        }
    }

    private static void lowerToken(Object value) {
        token(value);
        if (!LOWER_TOKEN.matcher((String) value).matches()) fail("invalid-event");
    }

    private static void text(Object value, int maxBytes) {
        if (!(value instanceof String string)
                || string.getBytes(StandardCharsets.UTF_8).length > maxBytes) {
            fail("invalid-event");
        }
    }

    private static void optionalText(Object value, int maxBytes) {
        if (value != null) text(value, maxBytes);
    }

    private static void valueBytes(Object value, int maxBytes) {
        if (Json.canonicalJson(value).getBytes(StandardCharsets.UTF_8).length > maxBytes) {
            fail("invalid-event");
        }
    }

    private static void validateScope(Object rawScope) {
        Map<?, ?> scope = asObject(rawScope, "invalid-scope");
        Object domain = scope.get("domain");
        if ("shared".equals(domain) || "backend".equals(domain)) {
            onlyKeys(scope, List.of("domain"), "invalid-scope");
            return;
        }
        if (!"contract".equals(domain)) fail("invalid-scope");
        onlyKeys(scope, List.of("domain", "contractHash"), "invalid-scope");
        Object hash = scope.get("contractHash");
        if (hash != null) {
            if (!(hash instanceof String text) || !CONTRACT_HASH.matcher(text).matches()) {
                fail("invalid-scope");
            }
        }
    }

    private static void validateIdentity(Object rawIdentity) {
        Map<?, ?> identity = asObject(rawIdentity, "invalid-event");
        onlyKeys(
            identity,
            List.of("oracle", "invariant", "kind", "message", "frame", "trigger", "boundary"),
            "invalid-event");
        lowerToken(identity.get("oracle"));
        for (String field : List.of("invariant", "kind", "message", "frame", "trigger")) {
            text(identity.get(field), MAX_TEXT_BYTES);
        }
        optionalText(identity.get("boundary"), MAX_TEXT_BYTES);
    }

    private static void validateEvent(Object rawEvent) {
        Map<?, ?> event = asObject(rawEvent, "invalid-event");
        Object kind = event.get("kind");
        if ("backend".equals(kind)) {
            onlyKeys(event, List.of("kind", "evidence"), "invalid-event");
            valueBytes(event.get("evidence"), MAX_CONTEXT_BYTES);
            return;
        }
        if ("graph-edge".equals(kind)) {
            onlyKeys(event, List.of("kind", "from", "action", "to"), "invalid-event");
            text(event.get("from"), MAX_TEXT_BYTES);
            text(event.get("action"), MAX_TEXT_BYTES);
            text(event.get("to"), MAX_TEXT_BYTES);
            return;
        }
        if (!"finding".equals(kind)) fail("invalid-event");
        onlyKeys(
            event,
            List.of("kind", "signature", "message", "identity", "path", "context"),
            "invalid-event");
        text(event.get("signature"), MAX_TEXT_BYTES);
        text(event.get("message"), MAX_TEXT_BYTES);
        validateIdentity(event.get("identity"));
        if (!(event.get("path") instanceof List<?> path) || path.size() > 256) {
            fail("invalid-event");
            return;
        }
        for (Object rawStep : path) {
            Map<?, ?> step = asObject(rawStep, "invalid-event");
            onlyKeys(step, List.of("signature", "action", "label"), "invalid-event");
            text(step.get("signature"), MAX_TEXT_BYTES);
            text(step.get("action"), MAX_TEXT_BYTES);
            optionalText(step.get("label"), MAX_TEXT_BYTES);
        }
        asObject(event.get("context"), "invalid-event");
        valueBytes(event.get("context"), MAX_CONTEXT_BYTES);
    }

    private static long validateFrame(Object rawFrame) {
        Map<?, ?> frame = asObject(rawFrame, "malformed-frame");
        onlyKeys(frame, List.of("runId", "sequence", "scope", "event"), "malformed-frame");
        token(frame.get("runId"));
        Object sequence = frame.get("sequence");
        boolean integral = sequence instanceof Integer || sequence instanceof Long;
        if (!integral || ((Number) sequence).longValue() < 0) fail("invalid-sequence");
        validateScope(frame.get("scope"));
        validateEvent(frame.get("event"));
        byte[] encoded = Json.canonicalJson(frame.get("event")).getBytes(StandardCharsets.UTF_8);
        if (encoded.length > MAX_FRAME_BYTES) fail("frame-too-large");
        return ((Number) sequence).longValue();
    }

    static void validateEventBatch(Object rawBatch) {
        Map<?, ?> batch = asObject(rawBatch, "malformed-frame");
        onlyKeys(
            batch,
            List.of("version", "batchId", "appId", "deployment", "frames", "evidence"),
            "invalid-event");
        Object version = batch.get("version");
        if (!(version instanceof Number number) || number.longValue() != 1) {
            fail("unsupported-version");
        }
        token(batch.get("batchId"));
        token(batch.get("appId"));
        Object deployment = batch.get("deployment");
        if (deployment != null) {
            Map<?, ?> fields = asObject(deployment, "invalid-event");
            onlyKeys(fields, List.of("version", "commit"), "invalid-event");
            if (fields.get("version") == null && fields.get("commit") == null) {
                fail("invalid-event");
            }
            if (fields.get("version") != null) token(fields.get("version"));
            if (fields.get("commit") != null) token(fields.get("commit"));
        }
        if (!(batch.get("frames") instanceof List<?> frames)
                || !(batch.get("evidence") instanceof List<?> evidence)) {
            fail("invalid-event");
            return;
        }
        if (frames.size() > MAX_BATCH_FRAMES) fail("batch-too-large");
        if (evidence.size() > MAX_BATCH_GRAPHS) fail("batch-too-large");
        if (frames.isEmpty() && evidence.isEmpty()) fail("invalid-event");
        Long lastSequence = null;
        for (Object frame : frames) {
            long sequence = validateFrame(frame);
            if (lastSequence != null && sequence <= lastSequence) fail("invalid-sequence");
            lastSequence = sequence;
        }
    }
}
