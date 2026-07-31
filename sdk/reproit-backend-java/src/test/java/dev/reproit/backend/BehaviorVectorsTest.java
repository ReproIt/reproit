package dev.reproit.backend;

// Shared behavioral conformance vectors (sdk/capture-behavior-v1.json).
//
// Eleven SDKs hand implement one contract, so a defect otherwise has to be
// found eleven times. Four instances of one class landed in a single day, and
// every group here was written against one of them.
//
// What each group pins, and the real defect behind it:
//
//   bounds                   the inline body budget is BYTES, not characters.
//                            4096 euro signs are 12288 bytes; a runtime
//                            measuring String.length() records that inline
//                            and blows a budget replay trusts. Java's budget
//                            is byte typed at the API, so the encoding is the
//                            caller's and the case pins it stays that way.
//   headers                  names lowercase, and the 32 header cap is taken
//                            over NAME SORTED order. Go capped a randomized
//                            map in arrival order and recorded a different
//                            subset every run, so replay was unrepeatable.
//   redaction typeCases      the placeholder carries type and length.
//   redaction foldingCases   which field names fold to secret.
//   redaction nestingCases   redaction reaches nested objects and arrays.
//   redaction structureCases redaction is structure preserving: no key
//                            dropped, no array shortened, an explicit null
//                            still a null value. An encoder that dropped null
//                            map values changed the shape the replay matcher
//                            walks, and replay reproduced a DIFFERENT error.

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import org.junit.jupiter.api.Test;

class BehaviorVectorsTest {

    @SuppressWarnings("unchecked")
    private static Map<String, Object> vectors() throws Exception {
        Path path = Path.of("..", "capture-behavior-v1.json").toAbsolutePath().normalize();
        String raw = Files.readString(path);
        return (Map<String, Object>) Json.parse(raw);
    }

    @SuppressWarnings("unchecked")
    @Test
    void constantsMatchTheSharedVectors() throws Exception {
        Map<String, Object> constants = (Map<String, Object>) vectors().get("constants");
        assertEquals(
                ((Number) constants.get("maxExchangeBodyBytes")).intValue(),
                Exchange.MAX_EXCHANGE_BODY_BYTES);
        assertEquals(constants.get("divergenceMarker"), Replay.DIVERGENCE_MARKER);
    }

    /** `bodyRepeat`/`repeat` keep the vectors small on disk. */
    private static String repeated(Object spec) {
        List<?> parts = (List<?>) spec;
        return String.valueOf(parts.get(0)).repeat(((Number) parts.get(1)).intValue());
    }

    @SuppressWarnings("unchecked")
    @Test
    void boundsVectors() throws Exception {
        Map<String, Object> bounds = (Map<String, Object>) vectors().get("bounds");
        for (Object entry : (List<Object>) bounds.get("cases")) {
            Map<String, Object> kase = (Map<String, Object>) entry;
            Map<String, Object> input = (Map<String, Object>) kase.get("input");
            String text = input.get("bodyRepeat") != null
                ? repeated(input.get("bodyRepeat"))
                : (String) input.get("body");
            byte[] body = text == null ? null : text.getBytes(StandardCharsets.UTF_8);
            Map<String, Object> expect =
                new LinkedHashMap<>((Map<String, Object>) kase.get("expect"));
            // A parsed JSON body is a Map too, so key on `repeat` itself.
            if (expect.get("body") instanceof Map<?, ?> repeat && repeat.get("repeat") != null) {
                expect.put("body", repeated(repeat.get("repeat")));
            }
            Object actual = Exchange.boundedBody(body, (String) input.get("contentType"));
            assertEquals(
                    Json.canonicalJson(expect),
                    Json.canonicalJson(actual),
                    "bounds case " + kase.get("name"));
        }
    }

    /**
     * The cap case is fed in a deterministic NON-sorted order, so a cap taken
     * over arrival order keeps the wrong subset and the assertion says so.
     */
    @SuppressWarnings("unchecked")
    @Test
    void headerVectors() throws Exception {
        Map<String, Object> headers = (Map<String, Object>) vectors().get("headers");
        for (Object entry : (List<Object>) headers.get("cases")) {
            Map<String, Object> kase = (Map<String, Object>) entry;
            if (kase.get("input") instanceof Map<?, ?> literal) {
                Map<String, String> given = new LinkedHashMap<>();
                Map<String, Object> table =
                    (Map<String, Object>) ((Map<String, Object>) literal).get("headers");
                table.forEach((name, value) -> given.put(name, String.valueOf(value)));
                assertEquals(
                        Json.canonicalJson(kase.get("expect")),
                        Json.canonicalJson(Exchange.boundedHeaders(given)),
                        "headers case " + kase.get("name"));
                continue;
            }
            Map<String, Object> spec = (Map<String, Object>) kase.get("inputGenerated");
            int count = ((Number) spec.get("headerCount")).intValue();
            Map<String, String> shuffled = new LinkedHashMap<>();
            for (int index = 0; index < count; index++) {
                // 17 is coprime with 40, so this walks every name exactly once.
                shuffled.put(
                        String.format((String) spec.get("namePattern"), (index * 17) % count),
                        (String) spec.get("value"));
            }
            Map<String, Object> expect = (Map<String, Object>) kase.get("expect");
            Map<String, Object> kept =
                (Map<String, Object>) Exchange.boundedHeaders(shuffled).get("headers");
            List<String> names = new ArrayList<>(kept.keySet());
            assertEquals(
                    ((Number) expect.get("headerCount")).intValue(),
                    names.size(),
                    "headers case " + kase.get("name"));
            assertEquals(
                    expect.get("firstName"),
                    names.get(0),
                    "the cap must be taken over sorted names, not arrival order");
            assertEquals(
                    expect.get("lastName"),
                    names.get(names.size() - 1),
                    "the cap must be taken over sorted names, not arrival order");
        }
    }

    @SuppressWarnings("unchecked")
    @Test
    void redactionTypeVectors() throws Exception {
        Map<String, Object> redaction = (Map<String, Object>) vectors().get("redaction");
        for (Object entry : (List<Object>) redaction.get("typeCases")) {
            Map<String, Object> kase = (Map<String, Object>) entry;
            Object actual = BackendTrace.redact(kase.get("input"));
            assertEquals(
                    Json.canonicalJson(kase.get("expect")),
                    Json.canonicalJson(actual),
                    String.valueOf(kase.get("input")));
        }
    }

    @SuppressWarnings("unchecked")
    @Test
    void redactionKeyFoldingVectors() throws Exception {
        Map<String, Object> redaction = (Map<String, Object>) vectors().get("redaction");
        for (Object entry : (List<Object>) redaction.get("foldingCases")) {
            Map<String, Object> kase = (Map<String, Object>) entry;
            String field = (String) kase.get("field");
            Object out = BackendTrace.redact(Map.of(field, "value"));
            Object value = ((Map<String, Object>) out).get(field);
            boolean redacted =
                    value instanceof Map<?, ?> map && map.containsKey("$reproit");
            assertEquals(kase.get("secret"), redacted, field);
        }
    }

    @SuppressWarnings("unchecked")
    @Test
    void redactionNestingVectors() throws Exception {
        Map<String, Object> redaction = (Map<String, Object>) vectors().get("redaction");
        for (Object entry : (List<Object>) redaction.get("nestingCases")) {
            Map<String, Object> kase = (Map<String, Object>) entry;
            assertEquals(
                    Json.canonicalJson(kase.get("expect")),
                    Json.canonicalJson(BackendTrace.redact(kase.get("input"))),
                    String.valueOf(kase.get("input")));
        }
    }

    /**
     * Structure preservation: a dropped key, a shortened array or a collapsed
     * null all change the shape the replay matcher walks.
     */
    @SuppressWarnings("unchecked")
    @Test
    void redactionStructureVectors() throws Exception {
        Map<String, Object> redaction = (Map<String, Object>) vectors().get("redaction");
        for (Object entry : (List<Object>) redaction.get("structureCases")) {
            Map<String, Object> kase = (Map<String, Object>) entry;
            assertEquals(
                    Json.canonicalJson(kase.get("expect")),
                    Json.canonicalJson(BackendTrace.redact(kase.get("input"))),
                    "structure case " + kase.get("name"));
        }
    }

    @SuppressWarnings("unchecked")
    @Test
    void triggerTokenIsInTheProtocolVocabulary() throws Exception {
        Map<String, Object> tokens = (Map<String, Object>) vectors().get("triggerTokens");
        Map<String, Object> bySdkKind = (Map<String, Object>) tokens.get("bySdkKind");
        String token = (String) bySdkKind.get("backend");
        assertTrue(((List<Object>) tokens.get("allowed")).contains(token));

        String source = Files.readString(
                Path.of("src/main/java/dev/reproit/backend/Capture.java"));
        assertTrue(source.contains(token), "Capture.java must emit " + token);
        for (Object bad : (List<Object>) tokens.get("rejected")) {
            assertFalse(
                    source.contains("\"" + bad + "\""),
                    "Capture.java must not emit " + bad);
        }
    }
}
