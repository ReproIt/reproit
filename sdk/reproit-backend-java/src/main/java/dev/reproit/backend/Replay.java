/*
 * Hermetic replay for reproit-backend-java.
 *
 * When `REPROIT_REPLAY` names a `reproit-backend-capture` payload, the same
 * boundary that records exchanges at capture time SERVES them instead, so
 * the application re-executes against exactly what production saw with no
 * live dependency at all.
 *
 * Determinism is a contract here, not a similarity score. Matching is
 * strict per-operation ordinals: within one operation (method plus path for
 * HTTP, statement text for pg) exchanges are consumed in recorded order, so
 * pooled db clients and LLM tool-call loops that interleave operations still
 * match exactly. Recorded `$reproit` redaction placeholders match any value
 * at their position, and a body truncated at capture fails closed. The first
 * unmatched call is a DIVERGENCE, reported as a structured
 * `REPROIT:DIVERGENCE` stderr line, byte-identical to the Node SDK's (the
 * report serializes in INSERTION order via {@link Json#orderedJson}, with a
 * `bodyDelta` naming WHERE the bodies differ; chat-shaped bodies name the
 * first differing message index).
 *
 * The envelope pins the replay: `TZ` and locale come from the capture,
 * {@link #clock()} is offset to the capture moment, and {@link #rng()} /
 * {@link #random()} yield the seeded stream. Honesty note: the seed makes
 * REPLAY runs deterministic; it does not reproduce the randomness the app
 * drew in production. Named no-weaving gaps: System.currentTimeMillis and
 * Instant.now cannot be intercepted without an agent (use the exposed
 * Clock), and Random instances the app constructs itself, Math.random, and
 * SecureRandom (unpinnable by design) stay live.
 */
package dev.reproit.backend;

import java.io.IOException;
import java.net.URI;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.TimeZone;

public final class Replay {
    /** The structured divergence marker, byte-identical to the Node SDK's. */
    public static final String DIVERGENCE_MARKER = "REPROIT:DIVERGENCE ";

    private static final class Entry {
        final Map<String, Object> exchange;
        boolean consumed;

        Entry(Map<String, Object> exchange) {
            this.exchange = exchange;
        }
    }

    private final Map<String, Object> envelope;
    private final List<Entry> exchanges = new ArrayList<>();

    private Replay(Map<String, Object> envelope, List<Map<String, Object>> exchanges) {
        this.envelope = envelope;
        for (Map<String, Object> exchange : exchanges) this.exchanges.add(new Entry(exchange));
    }

    /**
     * Load the capture named by `path`, or null when it is not a supported
     * `reproit-backend-capture` payload. Never throws on a bad file: replay
     * mode simply does not engage.
     */
    @SuppressWarnings("unchecked")
    static Replay load(String path) {
        try {
            String raw = Files.readString(Path.of(path), StandardCharsets.UTF_8);
            if (!(Json.parse(raw) instanceof Map<?, ?> parsed)) return null;
            Map<String, Object> payload = (Map<String, Object>) parsed;
            if (!"reproit-backend-capture".equals(payload.get("format"))) return null;
            long version = payload.get("version") instanceof Number number
                ? number.longValue() : 0;
            if (version < 1 || version > 2) return null;
            List<Map<String, Object>> found = new ArrayList<>();
            if (payload.get("events") instanceof List<?> events) {
                for (Object event : events) {
                    if (!(event instanceof Map<?, ?> map)) continue;
                    if (!"effect".equals(map.get("kind"))) continue;
                    if (map.get("exchange") instanceof Map<?, ?> exchange) {
                        found.add((Map<String, Object>) exchange);
                    }
                }
            }
            Map<String, Object> envelope = payload.get("envelope") instanceof Map<?, ?> map
                ? (Map<String, Object>) map : Map.of();
            return new Replay(envelope, found);
        } catch (IOException | RuntimeException unusable) {
            return null;
        }
    }

    /**
     * Pin process determinism from the capture envelope: default time zone,
     * default locale when recorded, and the clock offset {@link #clock()}
     * serves. System.currentTimeMillis itself cannot be intercepted without
     * bytecode weaving; that is a NAMED gap, and the SDK-provided Clock is
     * the supported source.
     */
    void pinEnvelope() {
        if (envelope.get("tz") instanceof String tz && !tz.isEmpty()) {
            TimeZone zone = TimeZone.getTimeZone(tz);
            // getTimeZone falls back to GMT for an unknown id; only pin a real one.
            if (tz.equals(zone.getID())) TimeZone.setDefault(zone);
        }
        if (envelope.get("locale") instanceof String tag && !tag.isEmpty()) {
            java.util.Locale locale = java.util.Locale.forLanguageTag(tag);
            if (!locale.toLanguageTag().equals("und")) java.util.Locale.setDefault(locale);
        }
        if (envelope.get("observedAtMs") instanceof Number observed) {
            clockOffsetMs = observed.longValue() - System.currentTimeMillis();
        }
    }

    private long clockOffsetMs = 0;

    /**
     * The replay clock: the system clock in the capture's zone, offset so the
     * moment replay pinned equals the capture moment.
     */
    public java.time.Clock clock() {
        return java.time.Clock.offset(
            java.time.Clock.system(TimeZone.getDefault().toZoneId()),
            java.time.Duration.ofMillis(clockOffsetMs));
    }

    /**
     * A java.util.Random over the seeded stream, so app code taking a Random
     * from the SDK replays deterministically. Every draw derives from the
     * same xorshift64* doubles the other SDKs pin. Null without a seed.
     */
    public java.util.Random random() {
        Rng stream = rng();
        return stream == null ? null : new SeededRandom(stream);
    }

    /** java.util.Random adapter over the envelope stream; reseeding is a no-op. */
    static final class SeededRandom extends java.util.Random {
        private transient Rng stream;

        SeededRandom(Rng stream) {
            super(0);
            this.stream = stream;
        }

        @Override
        public double nextDouble() {
            return stream.nextDouble();
        }

        @Override
        protected int next(int bits) {
            long draw = (long) (stream.nextDouble() * 4294967296.0);
            return (int) (draw >>> (32 - bits));
        }

        @Override
        public void setSeed(long seed) {
            // The envelope owns this stream; a library reseeding it would
            // silently break replay determinism. (Also called by the super
            // constructor, before `stream` is assigned.)
        }
    }

    /**
     * Deterministic xorshift64* stream from the capture's `replaySeed`, or
     * null when the capture carries no envelope seed.
     */
    public Rng rng() {
        if (!(envelope.get("replaySeed") instanceof String seed) || seed.isEmpty()) return null;
        String hex = seed.length() > 16 ? seed.substring(0, 16) : seed;
        try {
            return new Rng(Long.parseUnsignedLong(hex, 16) | 1L);
        } catch (NumberFormatException unusable) {
            return null;
        }
    }

    /** The seeded replay stream; matches the Node SDK's draw shape. */
    public static final class Rng {
        private long state;

        Rng(long state) {
            this.state = state;
        }

        /** The next draw in [0, 1). */
        public double nextDouble() {
            state ^= state << 13;
            state ^= state >>> 7;
            state ^= state << 17;
            long mixed = state * 0x2545f4914f6cdd1dL;
            return (mixed >>> 11) / (double) (1L << 53);
        }
    }

    /**
     * Strict per-operation ordinal match: the next unconsumed exchange of
     * THIS operation (method+path for http, statement text for pg) is the
     * only candidate; skipping it silently would be a fuzzy match. Other
     * operations' exchanges may interleave (pg pooling, tool-call loops),
     * which is why the key filters. Returns null on divergence, reported.
     */
    synchronized Map<String, Object> matched(String protocol, Map<String, Object> probe) {
        String key = operationKey(protocol, probe);
        for (Entry entry : exchanges) {
            if (entry.consumed || !protocol.equals(entry.exchange.get("protocol"))) continue;
            Map<String, Object> recorded = requestOf(entry);
            if (!operationKey(protocol, recorded).equals(key)) continue;
            boolean hit = "http".equals(protocol)
                ? httpMatches(recorded, probe)
                : dbMatches(recorded, probe);
            if (hit) {
                entry.consumed = true;
                return entry.exchange;
            }
            break;
        }
        diverge(protocol, probe);
        return null;
    }

    private static Map<String, Object> requestOf(Entry entry) {
        return entry.exchange.get("request") instanceof Map<?, ?> map ? castMap(map) : Map.of();
    }

    /** One operation's identity for ordinal matching. */
    static String operationKey(String protocol, Map<String, Object> request) {
        if ("http".equals(protocol)) {
            Object method = request.get("method");
            return (method == null ? "" : String.valueOf(method))
                + " " + pathAndQuery(request.get("url"));
        }
        Object text = request.get("text");
        return text == null ? "" : String.valueOf(text);
    }

    /**
     * Report a divergence on stderr in the shared structured shape. The line
     * is byte-compared against the Node reference by the parity suite, so
     * field order (insertion) and compact separators are load bearing.
     */
    synchronized void diverge(String protocol, Map<String, Object> probe) {
        String key = operationKey(protocol, probe);
        Map<String, Object> expected = null;
        Map<String, Object> firstCandidate = null;
        long consumed = 0;
        for (Entry entry : exchanges) {
            if (entry.consumed) {
                consumed += 1;
                continue;
            }
            if (!protocol.equals(entry.exchange.get("protocol"))) continue;
            Map<String, Object> recorded = requestOf(entry);
            if (firstCandidate == null) firstCandidate = recorded;
            if (expected == null && operationKey(protocol, recorded).equals(key)) {
                expected = recorded;
            }
        }
        if (expected == null) expected = firstCandidate;
        Map<String, Object> report = new LinkedHashMap<>();
        report.put("protocol", protocol);
        report.put("got", probe);
        report.put("expected", expected);
        report.put("consumed", consumed);
        report.put("total", (long) exchanges.size());
        // Prompt drift: when the recorded and live bodies both exist and
        // differ, name WHERE they differ. Chat-shaped bodies (OpenAI or
        // Anthropic messages arrays) name the first differing message index;
        // unknown shapes fall back to the first differing byte's offset.
        if (expected != null) {
            Object delta = bodyDelta(
                expected.containsKey("body") ? expected.get("body") : ABSENT,
                probe.containsKey("body") ? probe.get("body") : ABSENT);
            if (delta != null) report.put("bodyDelta", delta);
        }
        System.err.print(DIVERGENCE_MARKER + Json.orderedJson(report) + "\n");
    }

    /**
     * Sentinel for a body that is ABSENT from a request, as opposed to a
     * recorded JSON null (which the matcher wildcards). bodyDelta must not
     * report a delta when either side simply has no body.
     */
    static final Object ABSENT = new Object();

    /** The messages list of an OpenAI/Anthropic-shaped chat body, else null. */
    private static List<?> chatMessages(Object body) {
        if (body instanceof Map<?, ?> map && map.get("messages") instanceof List<?> list) {
            return list;
        }
        return null;
    }

    /**
     * Locate the first difference between a recorded request body and a live
     * one, modulo redaction placeholders. Null when there is nothing to
     * report (either body missing, or no difference the matcher objects to).
     */
    static Map<String, Object> bodyDelta(Object recorded, Object live) {
        if (recorded == ABSENT || live == ABSENT) return null;
        if (matches(recorded, live)) return null;
        List<?> recordedMessages = chatMessages(recorded);
        List<?> liveMessages = chatMessages(live);
        if (recordedMessages != null && liveMessages != null) {
            int bound = Math.min(recordedMessages.size(), liveMessages.size());
            Integer index = null;
            for (int i = 0; i < bound; i++) {
                if (!matches(recordedMessages.get(i), liveMessages.get(i))) {
                    index = i;
                    break;
                }
            }
            // All shared indexes match: the drift is a longer or shorter
            // conversation, and the first differing message is the first
            // unshared one. If lengths also agree the drift is outside
            // `messages`; fall through to bytes.
            if (index == null && recordedMessages.size() != liveMessages.size()) {
                index = bound;
            }
            if (index != null) {
                Map<String, Object> delta = new LinkedHashMap<>();
                delta.put("kind", "message");
                delta.put("firstDifferingMessage", (long) (int) index);
                delta.put("recordedMessages", (long) recordedMessages.size());
                delta.put("liveMessages", (long) liveMessages.size());
                return delta;
            }
        }
        byte[] recordedBytes = deltaBytes(recorded);
        byte[] liveBytes = deltaBytes(live);
        int bound = Math.min(recordedBytes.length, liveBytes.length);
        long offset = bound;
        for (int i = 0; i < bound; i++) {
            if (recordedBytes[i] != liveBytes[i]) {
                offset = i;
                break;
            }
        }
        Map<String, Object> delta = new LinkedHashMap<>();
        delta.put("kind", "byte");
        delta.put("offset", offset);
        return delta;
    }

    private static byte[] deltaBytes(Object value) {
        String text = value instanceof String string ? string : Json.orderedJson(value);
        return text.getBytes(StandardCharsets.UTF_8);
    }

    @SuppressWarnings("unchecked")
    private static Map<String, Object> castMap(Map<?, ?> map) {
        return (Map<String, Object>) map;
    }

    /**
     * Method, path and query of the original URL, and body modulo
     * placeholders. Recorded headers are deliberately not matched: they carry
     * per-run noise that would turn every replay into a divergence.
     */
    static boolean httpMatches(Map<String, Object> recorded, Map<String, Object> probe) {
        if (!java.util.Objects.equals(recorded.get("method"), probe.get("method"))) return false;
        if (!pathAndQuery(recorded.get("url")).equals(pathAndQuery(probe.get("url")))) {
            return false;
        }
        return !recorded.containsKey("body") || matches(recorded.get("body"), probe.get("body"));
    }

    /** Exact statement text, values modulo placeholders. */
    static boolean dbMatches(Map<String, Object> recorded, Map<String, Object> probe) {
        if (!java.util.Objects.equals(recorded.get("text"), probe.get("text"))) return false;
        return !recorded.containsKey("values")
            || matches(recorded.get("values"), probe.get("values"));
    }

    static String pathAndQuery(Object url) {
        if (!(url instanceof String text)) return "";
        try {
            URI parsed = URI.create(text);
            String path = parsed.getRawPath() == null ? "" : parsed.getRawPath();
            return parsed.getRawQuery() == null ? path : path + "?" + parsed.getRawQuery();
        } catch (IllegalArgumentException unparsed) {
            return text;
        }
    }

    /**
     * A recorded value matches a live one when equal, or when the recorded
     * side is a `$reproit` redaction placeholder (any value stood here at
     * capture). Objects compare per key; a recorded null matches anything.
     */
    static boolean matches(Object recorded, Object live) {
        if (recorded == null) return true;
        if (recorded instanceof Map<?, ?> map) {
            if (map.containsKey("$reproit")) return true;
            if (!(live instanceof Map<?, ?> liveMap)) return false;
            for (Map.Entry<?, ?> entry : map.entrySet()) {
                if (!matches(entry.getValue(), liveMap.get(entry.getKey()))) return false;
            }
            return true;
        }
        if (recorded instanceof List<?> list) {
            if (!(live instanceof List<?> liveList) || liveList.size() != list.size()) {
                return false;
            }
            for (int index = 0; index < list.size(); index++) {
                if (!matches(list.get(index), liveList.get(index))) return false;
            }
            return true;
        }
        if (recorded instanceof Number left && live instanceof Number right) {
            return left.doubleValue() == right.doubleValue();
        }
        return recorded.equals(live);
    }

    /**
     * One resolved HTTP serve: status, headers, whole body, and (for a
     * recorded stream shape) the body split at the recorded chunk boundaries
     * so the app observes the same number of chunks production did.
     */
    public record Served(
        int status, Map<String, String> headers, byte[] body, List<byte[]> chunks) {
        public String bodyText() {
            return new String(body, StandardCharsets.UTF_8);
        }
    }

    /**
     * Resolve a live HTTP probe against the session, entirely in process (no
     * sockets). A divergence and a body truncated at capture both serve a
     * hard 599 so the application observes an attributable failure instead
     * of a guess. Port of the Node reference's serveHttp.
     */
    public Served serveHttp(Map<String, Object> probe) {
        Map<String, Object> recorded = matched("http", probe);
        if (recorded == null) return diverged599("diverged");
        Map<String, Object> response = recorded.get("response") instanceof Map<?, ?> map
            ? castMap(map) : Map.of();
        if (Boolean.TRUE.equals(response.get("truncated"))) {
            // The capture kept identity but not bytes; serving a guessed body
            // would be a silent lie. Fail closed with the named reason.
            Map<String, Object> flagged = new LinkedHashMap<>(probe);
            flagged.put("truncated", Boolean.TRUE);
            diverge("http", flagged);
            return diverged599("truncated-exchange-body");
        }
        int status = response.get("status") instanceof Number number ? number.intValue() : 200;
        Map<String, String> headers = new LinkedHashMap<>();
        if (response.get("headers") instanceof Map<?, ?> recordedHeaders) {
            for (Map.Entry<?, ?> entry : recordedHeaders.entrySet()) {
                String name = String.valueOf(entry.getKey())
                    .toLowerCase(java.util.Locale.ROOT);
                if (name.equals("content-length") || name.equals("transfer-encoding")
                    || name.equals("content-encoding")) {
                    continue;
                }
                if (entry.getValue() != null) headers.put(name, String.valueOf(entry.getValue()));
            }
        }
        Object body = response.get("body");
        String bodyText;
        if (body == null && !response.containsKey("body")) {
            bodyText = "";
        } else if (body instanceof String text) {
            bodyText = text;
        } else {
            // Compact separators: byte-identical to the Node reference's
            // JSON.stringify of the same recorded body.
            bodyText = Json.orderedJson(body);
        }
        byte[] bytes = bodyText.getBytes(StandardCharsets.UTF_8);
        List<byte[]> chunks = null;
        if (response.get("stream") instanceof Map<?, ?> stream
                && stream.get("chunks") instanceof List<?> lengths) {
            if (Boolean.TRUE.equals(stream.get("truncated"))) {
                // The capture kept the body but not every chunk boundary;
                // serving a guessed stream shape would be a silent lie.
                Map<String, Object> flagged = new LinkedHashMap<>(probe);
                flagged.put("streamBoundariesTruncated", Boolean.TRUE);
                diverge("http", flagged);
                return diverged599("truncated-stream-boundaries");
            }
            chunks = splitChunks(bytes, lengths);
        }
        return new Served(status, headers, bytes, chunks);
    }

    /**
     * Split a replayed body at the recorded chunk boundaries (byte lengths).
     * Redaction can change body byte counts, so lengths are clamped and the
     * last chunk absorbs any remainder: the CHUNK COUNT (the stream shape the
     * app observed) is preserved exactly, the recorded content never padded.
     */
    static List<byte[]> splitChunks(byte[] body, List<?> lengths) {
        List<byte[]> chunks = new ArrayList<>(lengths.size());
        int offset = 0;
        for (int index = 0; index < lengths.size(); index++) {
            boolean last = index == lengths.size() - 1;
            long size = lengths.get(index) instanceof Number number
                && number.longValue() > 0 ? number.longValue() : 0;
            int end = last ? body.length : (int) Math.min(offset + size, body.length);
            chunks.add(java.util.Arrays.copyOfRange(body, offset, end));
            offset = end;
        }
        return chunks;
    }

    static Served diverged599(String reason) {
        Map<String, Object> body = new LinkedHashMap<>();
        body.put("reproit", reason);
        Map<String, String> headers = new LinkedHashMap<>();
        headers.put("content-type", "application/json");
        return new Served(
            599, headers, Json.orderedJson(body).getBytes(StandardCharsets.UTF_8), null);
    }

    /** Declared-JSON text parses to structure for matching; anything else stays text. */
    static Object tryJson(String text, String contentType) {
        if (contentType != null && contentType.contains("application/json")) {
            try {
                return Json.parse(text);
            } catch (RuntimeException notJson) {
                return text;
            }
        }
        return text;
    }
}
