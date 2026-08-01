/*
 * Bounded dependency-exchange values for reproit-backend-java.
 *
 * An exchange is the request the app sent to a dependency plus the response
 * that dependency returned. It is what hermetic replay serves, so responses
 * are recorded verbatim up to a fixed inline budget; an over-budget body
 * keeps only its byte count and sha256 and is marked truncated, and replay
 * fails closed on it with a named reason instead of guessing.
 *
 * Bounds are byte-identical to the Node and Rust SDKs so one replay engine
 * consumes every backend capture.
 */
package dev.reproit.backend;

import java.nio.charset.StandardCharsets;
import java.security.MessageDigest;
import java.security.NoSuchAlgorithmException;
import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Locale;
import java.util.Map;
import java.util.TreeMap;

public final class Exchange {
    /** Inline body budget per exchange side; beyond it only provable identity remains. */
    public static final int MAX_EXCHANGE_BODY_BYTES = 8 * 1024;
    /** Recorded headers are capped to keep events bounded. */
    public static final int MAX_EXCHANGE_HEADERS = 32;
    /** Rows recorded per db result; beyond it the result is marked truncated. */
    public static final int MAX_DB_ROWS = 64;
    /**
     * Stream chunk boundaries recorded per exchange (SSE / chunked responses,
     * the LLM streaming shape). Beyond it the boundaries are marked truncated
     * and replay fails closed rather than serve a wrong stream shape.
     */
    public static final int MAX_STREAM_CHUNKS = 128;

    private Exchange() {}

    /**
     * Collect a stream's chunks up to one byte past the inline budget; enough
     * to know the true size class without holding unbounded memory. The sha256
     * runs over EVERY byte so truncated identity stays provable. Chunk
     * boundaries are recorded as observed byte lengths, bounded by
     * MAX_STREAM_CHUNKS; boundaries past the cap are counted, never guessed.
     *
     * Java port of the Node reference's bodyCollector.
     */
    static final class BodyCollector {
        private final java.io.ByteArrayOutputStream inline =
            new java.io.ByteArrayOutputStream();
        private final List<Long> boundaries = new ArrayList<>();
        private final MessageDigest hash;
        private long bytes = 0;
        private long droppedBoundaries = 0;

        BodyCollector() {
            try {
                this.hash = MessageDigest.getInstance("SHA-256");
            } catch (NoSuchAlgorithmException impossible) {
                throw new IllegalStateException("SHA-256 unavailable", impossible);
            }
        }

        void push(byte[] chunk) {
            push(chunk, 0, chunk.length);
        }

        void push(byte[] chunk, int offset, int length) {
            bytes += length;
            hash.update(chunk, offset, length);
            if (boundaries.size() < MAX_STREAM_CHUNKS) {
                boundaries.add((long) length);
            } else {
                droppedBoundaries += 1;
            }
            if (bytes <= MAX_EXCHANGE_BODY_BYTES) inline.write(chunk, offset, length);
        }

        /**
         * The bounded body fields: empty when nothing arrived, provable
         * identity when over budget, the raw text (JSON parsed when declared)
         * otherwise. Mirrors boundedBody over the collected stream.
         */
        Map<String, Object> body(String contentType) {
            if (bytes == 0) return new LinkedHashMap<>();
            if (bytes > MAX_EXCHANGE_BODY_BYTES) {
                Instrument.countTruncatedBody();
                Map<String, Object> fields = new LinkedHashMap<>();
                fields.put("bodyBytes", bytes);
                fields.put("bodySha256", hexOf(hash.digest()));
                fields.put("truncated", Boolean.TRUE);
                return fields;
            }
            return boundedBody(inline.toByteArray(), contentType);
        }

        boolean truncated() {
            return bytes > MAX_EXCHANGE_BODY_BYTES;
        }

        /**
         * Chunk boundaries as observed byte lengths. Recorded when the
         * response is a stream (SSE always; anything else only when it
         * actually arrived in more than one chunk, since a single-chunk body
         * replays identically without them). Null when not worth recording.
         */
        Map<String, Object> stream(boolean isEventStream) {
            if (boundaries.isEmpty()) return null;
            if (!isEventStream && boundaries.size() < 2 && droppedBoundaries == 0) return null;
            Map<String, Object> fields = new LinkedHashMap<>();
            fields.put("chunks", new ArrayList<>(boundaries));
            if (droppedBoundaries > 0) fields.put("truncated", Boolean.TRUE);
            return fields;
        }
    }

    /**
     * Bound one exchange body: within budget it is recorded verbatim (JSON
     * parsed when the content type declares it), beyond it only byte count,
     * sha256, and the truncated marker.
     */
    static Map<String, Object> boundedBody(byte[] body, String contentType) {
        Map<String, Object> fields = new LinkedHashMap<>();
        if (body == null || body.length == 0) return fields;
        if (body.length > MAX_EXCHANGE_BODY_BYTES) {
            Instrument.countTruncatedBody();
            fields.put("bodyBytes", (long) body.length);
            fields.put("bodySha256", sha256Hex(body));
            fields.put("truncated", Boolean.TRUE);
            return fields;
        }
        String text = new String(body, StandardCharsets.UTF_8);
        if (contentType != null && contentType.contains("application/json")) {
            try {
                fields.put("body", Json.parse(text));
                return fields;
            } catch (RuntimeException notJson) {
                // Declared JSON that does not parse is recorded as text below.
            }
        }
        fields.put("body", text);
        return fields;
    }

    /**
     * Lowercased header names, capped over NAME SORTED order; absent when
     * empty. Sorting before the cap is the contract: a map iterated in
     * arrival order records a different subset per run, so two runs of one
     * request disagree and the capsule stops matching.
     */
    static Map<String, Object> boundedHeaders(Map<String, String> headers) {
        Map<String, Object> fields = new LinkedHashMap<>();
        if (headers == null || headers.isEmpty()) return fields;
        Map<String, String> sorted = new TreeMap<>();
        for (Map.Entry<String, String> entry : headers.entrySet()) {
            if (entry.getKey() == null || entry.getValue() == null) continue;
            sorted.put(entry.getKey().toLowerCase(Locale.ROOT), entry.getValue());
        }
        Map<String, Object> capped = new LinkedHashMap<>();
        for (Map.Entry<String, String> entry : sorted.entrySet()) {
            if (capped.size() >= MAX_EXCHANGE_HEADERS) break;
            capped.put(entry.getKey(), entry.getValue());
        }
        if (!capped.isEmpty()) fields.put("headers", capped);
        return fields;
    }

    /** The recorded shape of one HTTP exchange. */
    static Map<String, Object> http(
            String method,
            String url,
            Map<String, String> requestHeaders,
            byte[] requestBody,
            String requestContentType,
            int status,
            Map<String, String> responseHeaders,
            byte[] responseBody,
            String responseContentType) {
        return http(
            method, url, requestHeaders, requestBody, requestContentType,
            status, responseHeaders, boundedBody(responseBody, responseContentType), null);
    }

    /**
     * The recorded shape of one HTTP exchange, response fields pre-bounded (a
     * BodyCollector's `body()`), with an optional observed stream shape.
     * Boundaries are only kept for bodies recorded verbatim: a truncated
     * inline body already fails closed at replay.
     */
    static Map<String, Object> http(
            String method,
            String url,
            Map<String, String> requestHeaders,
            byte[] requestBody,
            String requestContentType,
            int status,
            Map<String, String> responseHeaders,
            Map<String, Object> responseBody,
            Map<String, Object> stream) {
        Map<String, Object> request = new LinkedHashMap<>();
        request.put("method", method);
        request.put("url", url);
        request.putAll(boundedHeaders(requestHeaders));
        request.putAll(boundedBody(requestBody, requestContentType));
        Map<String, Object> response = new LinkedHashMap<>();
        response.put("status", (long) status);
        response.putAll(boundedHeaders(responseHeaders));
        response.putAll(responseBody);
        if (stream != null && !Boolean.TRUE.equals(responseBody.get("truncated"))) {
            response.put("stream", stream);
        }
        Map<String, Object> exchange = new LinkedHashMap<>();
        exchange.put("protocol", "http");
        exchange.put("request", request);
        exchange.put("response", response);
        return exchange;
    }

    /** The recorded shape of one database exchange. */
    static Map<String, Object> db(String text, List<Object> values, Map<String, Object> outcome) {
        Map<String, Object> request = new LinkedHashMap<>();
        request.put("text", text == null ? "" : text);
        if (values != null && !values.isEmpty()) request.put("values", new ArrayList<>(values));
        Map<String, Object> exchange = new LinkedHashMap<>();
        exchange.put("protocol", "pg");
        exchange.put("request", request);
        exchange.put("response", outcome);
        return exchange;
    }

    /** Rows beyond the cap are dropped and the outcome is marked truncated. */
    static Map<String, Object> dbOutcome(String command, long rowCount, List<Object> rows) {
        Map<String, Object> outcome = new LinkedHashMap<>();
        outcome.put("command", command);
        outcome.put("rowCount", rowCount);
        List<Object> kept = rows == null ? List.of() : rows;
        boolean truncated = kept.size() > MAX_DB_ROWS;
        outcome.put(
            "rows", new ArrayList<>(kept.subList(0, Math.min(kept.size(), MAX_DB_ROWS))));
        if (truncated) outcome.put("truncated", Boolean.TRUE);
        return outcome;
    }

    static Map<String, Object> dbError(String message, String code) {
        Map<String, Object> error = new LinkedHashMap<>();
        error.put("message", message);
        error.put("code", code);
        Map<String, Object> outcome = new LinkedHashMap<>();
        outcome.put("error", error);
        return outcome;
    }

    /**
     * Effect kind for a SQL statement: reads stay reads so state oracles keep
     * their meaning; everything else is a write.
     */
    static String dbEffectKind(String text) {
        String verb = (text == null ? "" : text).stripLeading();
        verb = verb.substring(0, Math.min(verb.length(), 8)).toUpperCase(Locale.ROOT);
        return verb.startsWith("SELECT") || verb.startsWith("SHOW") ? "read" : "write";
    }

    static String sha256Hex(byte[] bytes) {
        try {
            return hexOf(MessageDigest.getInstance("SHA-256").digest(bytes));
        } catch (NoSuchAlgorithmException impossible) {
            throw new IllegalStateException("SHA-256 unavailable", impossible);
        }
    }

    static String hexOf(byte[] digest) {
        StringBuilder out = new StringBuilder(digest.length * 2);
        for (byte value : digest) out.append(String.format("%02x", value));
        return out.toString();
    }
}
