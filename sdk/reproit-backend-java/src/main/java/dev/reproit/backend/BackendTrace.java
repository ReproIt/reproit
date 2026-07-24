/*
 * reproit-backend-java, experimental backend trace adapter (v0.0.0)
 *
 * Java port of sdk/reproit-backend-rs. Scan-time: services activate this
 * adapter only when a trusted request carries `x-reproit-trace`. The resulting
 * response header (`x-reproit-events`) contains bounded, trace-bound,
 * structurally redacted events. Production: the optional, config-gated capture
 * mode (Capture.java) self-samples finished traces (always on 5xx / failure,
 * optional healthy baseline) and posts them to Cloud ingest. It is not a
 * public compatibility surface while backend contracts remain experimental.
 *
 * Wire parity with the Rust adapter: events serialize as compact JSON with
 * recursively sorted keys (serde_json's BTreeMap order), and the header is
 * unpadded base64url of that encoding.
 */
package dev.reproit.backend;

import java.nio.charset.StandardCharsets;
import java.security.MessageDigest;
import java.security.NoSuchAlgorithmException;
import java.util.ArrayList;
import java.util.Base64;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Locale;
import java.util.Map;
import java.util.concurrent.atomic.AtomicLong;
import java.util.function.Function;
import java.util.regex.Pattern;

public final class BackendTrace {
    public static final int MAX_EVENTS = 256;
    public static final int MAX_HEADER_BYTES = 60000;
    static final List<String> EFFECT_KINDS = List.of("read", "write", "delete", "emit", "call");

    private static final AtomicLong SEQUENCE = new AtomicLong(1);
    private static final Pattern PATH_SEGMENT = Pattern.compile("^[A-Za-z_][A-Za-z0-9_]*$");
    private static final List<String> SECRET_PARTS = List.of(
        "password", "passwd", "secret", "token", "authorization", "cookie", "email", "phone",
        "apikey", "publishablekey", "privatekey", "accesskey", "signingkey", "idempotencykey");

    /** Optional begin() inputs; plain fields with chainable setters. */
    public static final class Options {
        String spanId;
        String tenant;
        String idempotencyKey;
        Object input;
        List<Map<String, Object>> selections;

        public Options spanId(String value) { this.spanId = value; return this; }
        public Options tenant(String value) { this.tenant = value; return this; }
        public Options idempotencyKey(String value) { this.idempotencyKey = value; return this; }
        public Options input(Object value) { this.input = value; return this; }
        public Options selections(List<Map<String, Object>> value) {
            this.selections = value;
            return this;
        }
    }

    /** Optional effect() inputs; plain fields with chainable setters. */
    public static final class Effect {
        String resource;
        String key;
        String tenant;
        String event;
        Object detail;

        public Effect resource(String value) { this.resource = value; return this; }
        public Effect key(String value) { this.key = value; return this; }
        public Effect tenant(String value) { this.tenant = value; return this; }
        public Effect event(String value) { this.event = value; return this; }
        public Effect detail(Object value) { this.detail = value; return this; }
    }

    private final Map<String, Object> common;
    private final List<Map<String, Object>> events = new ArrayList<>();
    private boolean finished = false;

    private BackendTrace(Map<String, Object> common) {
        this.common = common;
    }

    // Trimmed, non-empty, at most `maximum` code points; null otherwise.
    static String bounded(String value, int maximum) {
        if (value == null) return null;
        String trimmed = value.strip();
        if (trimmed.isEmpty()) return null;
        if (trimmed.codePointCount(0, trimmed.length()) > maximum) return null;
        return trimmed;
    }

    /**
     * `get(name)` returns the request header value (or null). Returns null
     * when no valid `x-reproit-trace` is present: the adapter stays inert.
     */
    public static TraceContext traceContextFromHeaders(Function<String, String> get) {
        String traceId = bounded(get.apply("x-reproit-trace"), 128);
        if (traceId == null) return null;
        long actionIndex = 0;
        String rawAction = get.apply("x-reproit-action");
        if (rawAction != null) {
            try {
                long parsed = Long.parseLong(rawAction.strip());
                if (parsed >= 0 && parsed <= 0xffffffffL) actionIndex = parsed;
            } catch (NumberFormatException ignored) {
                // A malformed action index falls back to 0, like the reference.
            }
        }
        return new TraceContext(
            traceId,
            bounded(get.apply("x-reproit-actor"), 32),
            actionIndex,
            bounded(get.apply("x-reproit-build"), 128),
            bounded(get.apply("x-reproit-config-contract"), 128));
    }

    private static boolean validPath(String path) {
        if (path == null || path.isEmpty()) return false;
        for (String segment : path.split("\\.", -1)) {
            String name = segment.endsWith("[]")
                ? segment.substring(0, segment.length() - 2)
                : segment;
            if (!PATH_SEGMENT.matcher(name).matches()) return false;
        }
        return true;
    }

    /** GraphQL selection mapping (parser-produced only); null when invalid. */
    public static Map<String, Object> selection(
            String schemaPath, String responsePath, String typeCondition) {
        if (!validPath(schemaPath) || !validPath(responsePath)) return null;
        Map<String, Object> value = new LinkedHashMap<>();
        value.put("schemaPath", schemaPath);
        value.put("responsePath", responsePath);
        if (typeCondition != null) {
            boolean invalid = !validPath(typeCondition)
                || typeCondition.contains(".") || typeCondition.contains("[]");
            if (invalid) return null;
            value.put("typeCondition", typeCondition);
        }
        return value;
    }

    /**
     * Canonical decoded OpenAPI input. Framework adapters must provide decoded
     * values (including lists for repeated query/header parameters), never raw
     * query strings whose serialization style is ambiguous.
     */
    public static Map<String, Object> httpInput(
            Object body,
            Map<String, Object> path,
            Map<String, Object> query,
            Map<String, Object> headers) {
        Map<String, Object> value = new LinkedHashMap<>();
        if (body != null) value.put("body", body);
        putFields(value, "path", path, false);
        putFields(value, "query", query, false);
        putFields(value, "headers", headers, true);
        return value;
    }

    private static void putFields(
            Map<String, Object> value, String name, Map<String, Object> fields, boolean lower) {
        if (fields == null || fields.isEmpty()) return;
        Map<String, Object> copied = new LinkedHashMap<>();
        for (Map.Entry<String, Object> entry : fields.entrySet()) {
            String key = lower ? entry.getKey().toLowerCase(Locale.ROOT) : entry.getKey();
            copied.put(key, entry.getValue());
        }
        value.put(name, copied);
    }

    // Hashed identity for idempotency keys: never ship the raw key.
    static String identity(String value) {
        try {
            MessageDigest digest = MessageDigest.getInstance("SHA-256");
            byte[] hash = digest.digest(value.getBytes(StandardCharsets.UTF_8));
            StringBuilder out = new StringBuilder("sha256:");
            for (int index = 0; index < 12; index++) {
                out.append(String.format("%02x", hash[index]));
            }
            return out.toString();
        } catch (NoSuchAlgorithmException impossible) {
            throw new IllegalStateException("SHA-256 unavailable", impossible);
        }
    }

    static boolean secretField(String name) {
        StringBuilder folded = new StringBuilder();
        for (int index = 0; index < name.length(); index++) {
            char ch = name.charAt(index);
            boolean keep = (ch >= 'a' && ch <= 'z') || (ch >= 'A' && ch <= 'Z')
                || (ch >= '0' && ch <= '9');
            if (keep) folded.append(Character.toLowerCase(ch));
        }
        String haystack = folded.toString();
        for (String part : SECRET_PARTS) {
            if (haystack.contains(part)) return true;
        }
        return false;
    }

    /**
     * Recursive structural redaction: secret-named fields become `$reproit`
     * metadata stubs (type + length), everything else recurses.
     */
    public static Object redact(Object value) {
        if (value instanceof Map<?, ?> map) {
            Map<String, Object> out = new LinkedHashMap<>();
            for (Map.Entry<?, ?> entry : map.entrySet()) {
                String key = String.valueOf(entry.getKey());
                Object field = entry.getValue();
                out.put(key, secretField(key) ? metadata(field) : redact(field));
            }
            return out;
        }
        if (value instanceof List<?> list) {
            List<Object> out = new ArrayList<>(list.size());
            for (Object item : list) out.add(redact(item));
            return out;
        }
        return value;
    }

    private static Map<String, Object> metadata(Object value) {
        String kind = "null";
        Object length = null;
        if (value instanceof Boolean) {
            kind = "boolean";
        } else if (value instanceof Double || value instanceof Float) {
            kind = "number";
        } else if (value instanceof Number) {
            kind = "integer";
        } else if (value instanceof CharSequence text) {
            kind = "string";
            String string = text.toString();
            length = (long) string.codePointCount(0, string.length());
        } else if (value instanceof List<?> list) {
            kind = "array";
            length = (long) list.size();
        } else if (value instanceof Map) {
            kind = "object";
        }
        Map<String, Object> stub = new LinkedHashMap<>();
        stub.put("redacted", Boolean.TRUE);
        stub.put("type", kind);
        stub.put("length", length);
        return Map.of("$reproit", stub);
    }

    public static BackendTrace begin(TraceContext context, String operation, Options opts) {
        if (opts == null) opts = new Options();
        String name = bounded(operation, 256);
        if (name == null) throw new TraceError("InvalidOperation");
        String spanId = bounded(
            opts.spanId != null ? opts.spanId : context.traceId() + ":" + name, 128);
        if (spanId == null) throw new TraceError("InvalidOperation");
        Map<String, Object> common = new LinkedHashMap<>();
        common.put("traceId", context.traceId());
        common.put("spanId", spanId);
        common.put("actionIndex", context.actionIndex());
        common.put("operation", name);
        if (context.actor() != null) common.put("actor", context.actor());
        if (context.build() != null) common.put("build", context.build());
        if (context.configContract() != null) {
            common.put("configContract", context.configContract());
        }
        String tenant = opts.tenant != null ? bounded(opts.tenant, 128) : null;
        if (tenant != null) common.put("tenant", tenant);
        if (opts.idempotencyKey != null) {
            common.put("idempotencyKey", identity(opts.idempotencyKey));
        }
        if (opts.selections != null && !opts.selections.isEmpty()) {
            int take = Math.min(opts.selections.size(), MAX_EVENTS);
            common.put("selections", new ArrayList<>(opts.selections.subList(0, take)));
        }
        BackendTrace trace = new BackendTrace(common);
        Map<String, Object> fields = new LinkedHashMap<>();
        fields.put("input", redact(opts.input));
        trace.push("start", fields);
        return trace;
    }

    public void effect(String kind, Effect opts) {
        if (opts == null) opts = new Effect();
        if (finished) throw new TraceError("AlreadyFinished");
        if (!EFFECT_KINDS.contains(kind)) throw new TraceError("InvalidOperation");
        Map<String, Object> fields = new LinkedHashMap<>();
        fields.put("effect", kind);
        putTruncated(fields, "resource", opts.resource);
        putTruncated(fields, "key", opts.key);
        putTruncated(fields, "effectTenant", opts.tenant);
        putTruncated(fields, "event", opts.event);
        if (opts.detail != null && redact(opts.detail) instanceof Map<?, ?> detail) {
            for (String key : List.of("before", "after", "payload")) {
                if (detail.containsKey(key)) fields.put(key, detail.get(key));
            }
        }
        push("effect", fields);
    }

    private static void putTruncated(Map<String, Object> fields, String name, String value) {
        if (value == null) return;
        if (value.codePointCount(0, value.length()) > 256) {
            value = value.substring(0, value.offsetByCodePoints(0, 256));
        }
        fields.put(name, value);
    }

    public void finish(Object output, Integer status, boolean success, boolean effectsComplete) {
        if (finished) throw new TraceError("AlreadyFinished");
        Map<String, Object> fields = new LinkedHashMap<>();
        fields.put("output", redact(output));
        fields.put("status", status);
        fields.put("success", success);
        fields.put("effectsComplete", effectsComplete);
        push("return", fields);
        finished = true;
    }

    public String header() {
        if (!finished) throw new TraceError("AlreadyFinished");
        byte[] raw = Json.canonicalJson(events).getBytes(StandardCharsets.UTF_8);
        String encoded = Base64.getUrlEncoder().withoutPadding().encodeToString(raw);
        if (encoded.length() > MAX_HEADER_BYTES) throw new TraceError("HeaderTooLarge");
        return encoded;
    }

    public List<Map<String, Object>> events() {
        return events;
    }

    public boolean finished() {
        return finished;
    }

    private void push(String kind, Map<String, Object> fields) {
        if (events.size() >= MAX_EVENTS) throw new TraceError("TooManyEvents");
        Map<String, Object> event = new LinkedHashMap<>(common);
        event.put("sequence", SEQUENCE.getAndIncrement());
        event.put("kind", kind);
        event.putAll(fields);
        events.add(event);
    }
}
