/*
 * Hermetic replay for reproit-backend-java.
 *
 * When `REPROIT_REPLAY` names a `reproit-backend-capture` payload, the same
 * boundary that records exchanges at capture time SERVES them instead, so
 * the application re-executes against exactly what production saw with no
 * live dependency at all.
 *
 * Determinism is a contract here, not a similarity score. Matching is
 * strict: the first unconsumed exchange of the protocol is the only
 * candidate, recorded `$reproit` redaction placeholders match any value at
 * their position, and a body truncated at capture fails closed. The first
 * unmatched call is a DIVERGENCE, reported as a structured
 * `REPROIT:DIVERGENCE` stderr line, byte-identical to the Node SDK's.
 *
 * The envelope pins the replay: `TZ` comes from the capture and
 * {@link #rng()} yields the seeded stream. Honesty note: the seed makes
 * REPLAY runs deterministic; it does not reproduce the randomness the app
 * drew in production.
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

    /** Pin process determinism from the capture envelope. */
    void pinEnvelope() {
        if (envelope.get("tz") instanceof String tz && !tz.isEmpty()) {
            TimeZone zone = TimeZone.getTimeZone(tz);
            // getTimeZone falls back to GMT for an unknown id; only pin a real one.
            if (tz.equals(zone.getID())) TimeZone.setDefault(zone);
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
     * Strict next-unconsumed match. The first unconsumed exchange of the
     * protocol is the ONLY candidate; skipping it silently would be a fuzzy
     * match. Returns null on divergence, already reported.
     */
    synchronized Map<String, Object> matched(String protocol, Map<String, Object> probe) {
        for (Entry entry : exchanges) {
            if (entry.consumed || !protocol.equals(entry.exchange.get("protocol"))) continue;
            Object request = entry.exchange.get("request");
            Map<String, Object> recorded = request instanceof Map<?, ?> map
                ? castMap(map) : Map.of();
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

    /** Report a divergence on stderr in the shared structured shape. */
    synchronized void diverge(String protocol, Map<String, Object> probe) {
        Object expected = null;
        long consumed = 0;
        for (Entry entry : exchanges) {
            if (entry.consumed) {
                consumed += 1;
            } else if (expected == null && protocol.equals(entry.exchange.get("protocol"))) {
                expected = entry.exchange.get("request");
            }
        }
        Map<String, Object> report = new LinkedHashMap<>();
        report.put("protocol", protocol);
        report.put("got", probe);
        report.put("expected", expected);
        report.put("consumed", consumed);
        report.put("total", (long) exchanges.size());
        System.err.println(DIVERGENCE_MARKER + Json.canonicalJson(report));
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
    private static boolean httpMatches(Map<String, Object> recorded, Map<String, Object> probe) {
        if (!java.util.Objects.equals(recorded.get("method"), probe.get("method"))) return false;
        if (!pathAndQuery(recorded.get("url")).equals(pathAndQuery(probe.get("url")))) {
            return false;
        }
        return !recorded.containsKey("body") || matches(recorded.get("body"), probe.get("body"));
    }

    /** Exact statement text, values modulo placeholders. */
    private static boolean dbMatches(Map<String, Object> recorded, Map<String, Object> probe) {
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
}
