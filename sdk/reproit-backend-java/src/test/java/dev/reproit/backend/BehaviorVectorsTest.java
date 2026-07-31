package dev.reproit.backend;

// Shared behavioral conformance vectors (sdk/capture-behavior-v1.json).
//
// Eleven SDKs hand implement one contract, so a defect otherwise has to be
// found eleven times. Four instances of one class landed in a single day, and
// every group here was written against one of them.

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.nio.file.Files;
import java.nio.file.Path;
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
