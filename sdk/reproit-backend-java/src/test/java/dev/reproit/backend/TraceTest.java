// Semantics parity tests against sdk/reproit-backend-rs/src/lib.rs, ported
// from sdk/reproit-backend-node/test/trace.test.js.
package dev.reproit.backend;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNotEquals;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.Base64;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import org.junit.jupiter.api.Test;

class TraceTest {
    private static TraceContext context() {
        return new TraceContext("trace-a", null, 0, null, null);
    }

    @SuppressWarnings("unchecked")
    private static Map<String, Object> at(Object value, String... path) {
        Map<String, Object> current = (Map<String, Object>) value;
        for (String key : path) {
            current = (Map<String, Object>) current.get(key);
        }
        return current;
    }

    @Test
    void emitsBoundedCorrelatedRedactedEvents() {
        Map<String, String> headers = Map.of(
            "x-reproit-trace", "trace-a",
            "x-reproit-actor", "alice",
            "x-reproit-action", "7",
            "x-reproit-build", "build-a",
            "x-reproit-config-contract", "contract-a");
        TraceContext parsed = BackendTrace.traceContextFromHeaders(headers::get);
        Map<String, Object> input = new LinkedHashMap<>();
        input.put("name", "demo");
        input.put("password", "abcdefgh");
        BackendTrace trace = BackendTrace.begin(parsed, "createProject",
            new BackendTrace.Options()
                .tenant("org-1")
                .idempotencyKey("retry-secret")
                .input(input)
                .selections(List.of(BackendTrace.selection("project.id", "projectId", null))));
        trace.effect("write",
            new BackendTrace.Effect().resource("projects").key("1").tenant("org-1"));
        Map<String, Object> output = new LinkedHashMap<>();
        output.put("id", 1L);
        output.put("apiKey", "sk_live_secret");
        output.put("publishable_key", "pk_live_secret");
        output.put("private-key", "private-secret");
        output.put("access key", "access-secret");
        output.put("signingKey", "signing-secret");
        output.put("monkey", "harmless");
        trace.finish(output, 201, true, true);
        assertTrue(trace.header().length() < BackendTrace.MAX_HEADER_BYTES);
        List<Map<String, Object>> events = trace.events();
        assertEquals(7L, events.get(0).get("actionIndex"));
        assertEquals("build-a", events.get(0).get("build"));
        assertEquals("contract-a", events.get(0).get("configContract"));
        assertEquals(8L, at(events.get(0), "input", "password", "$reproit").get("length"));
        assertNotEquals("retry-secret", events.get(0).get("idempotencyKey"));
        assertTrue(((String) events.get(0).get("idempotencyKey"))
            .matches("^sha256:[0-9a-f]{24}$"));
        for (String field : List.of(
                "apiKey", "publishable_key", "private-key", "access key", "signingKey")) {
            assertEquals(true, at(events.get(2), "output", field, "$reproit").get("redacted"));
        }
        assertEquals("harmless", at(events.get(2), "output").get("monkey"));
        assertEquals(true, events.get(2).get("effectsComplete"));
    }

    @Test
    void staysInactiveWithoutATraceHeader() {
        assertNull(BackendTrace.traceContextFromHeaders(name -> null));
        assertNull(BackendTrace.traceContextFromHeaders(
            name -> name.equals("x-reproit-trace") ? "  " : null));
    }

    @Test
    void headerIsUnpaddedBase64urlOfTheCanonicalEventJson() {
        Map<String, Object> input = new LinkedHashMap<>();
        input.put("b", 1L);
        input.put("a", 2L);
        BackendTrace trace = BackendTrace.begin(
            context(), "op", new BackendTrace.Options().input(input));
        trace.finish(Map.of("ok", true), 200, true, true);
        String header = trace.header();
        assertFalse(header.contains("+") || header.contains("/") || header.contains("="));
        String raw = new String(Base64.getUrlDecoder().decode(header), StandardCharsets.UTF_8);
        assertEquals(Json.canonicalJson(trace.events()), raw);
        // Keys are sorted (serde_json BTreeMap order in the Rust adapter).
        assertTrue(raw.indexOf("\"a\":2") < raw.indexOf("\"b\":1"));
    }

    @Test
    void rejectsEffectsAfterReturnAndASecondReturn() {
        BackendTrace trace = BackendTrace.begin(context(), "op", null);
        trace.finish(null, 200, true, false);
        TraceError effect = assertThrows(TraceError.class,
            () -> trace.effect("read", null));
        assertEquals("AlreadyFinished", effect.code);
        TraceError second = assertThrows(TraceError.class,
            () -> trace.finish(null, 200, true, false));
        assertEquals("AlreadyFinished", second.code);
    }

    @Test
    void headerBeforeFinishIsRejectedOversizedHeaderIsRejected() {
        BackendTrace open = BackendTrace.begin(context(), "op", null);
        assertEquals("AlreadyFinished", assertThrows(TraceError.class, open::header).code);
        BackendTrace big = BackendTrace.begin(context(), "op", null);
        big.finish(Map.of("blob", "x".repeat(BackendTrace.MAX_HEADER_BYTES)), 200, true, true);
        assertEquals("HeaderTooLarge", assertThrows(TraceError.class, big::header).code);
    }

    @Test
    void eventCountIsCappedAt256() {
        BackendTrace trace = BackendTrace.begin(context(), "op", null);
        for (int index = 1; index < BackendTrace.MAX_EVENTS; index++) {
            trace.effect("emit", new BackendTrace.Effect().event("tick"));
        }
        assertEquals("TooManyEvents",
            assertThrows(TraceError.class, () -> trace.effect("emit", null)).code);
        assertEquals("TooManyEvents",
            assertThrows(TraceError.class, () -> trace.finish(null, 200, true, false)).code);
    }

    @Test
    void typedEffectsOnlyBoundedIdentifiersOnly() {
        BackendTrace trace = BackendTrace.begin(context(), "op", null);
        assertEquals("InvalidOperation",
            assertThrows(TraceError.class, () -> trace.effect("mutate", null)).code);
        assertEquals("InvalidOperation",
            assertThrows(TraceError.class, () -> BackendTrace.begin(context(), "", null)).code);
        assertEquals("InvalidOperation",
            assertThrows(TraceError.class,
                () -> BackendTrace.begin(context(), "x".repeat(257), null)).code);
    }

    @Test
    void effectDetailKeepsOnlyBeforeAfterPayloadAfterRedaction() {
        BackendTrace trace = BackendTrace.begin(context(), "op", null);
        Map<String, Object> detail = new LinkedHashMap<>();
        detail.put("before", Map.of("email", "a@b.c"));
        detail.put("after", Map.of("name", "z"));
        detail.put("extra", "dropped");
        trace.effect("write", new BackendTrace.Effect().resource("users").detail(detail));
        Map<String, Object> effect = trace.events().get(1);
        assertEquals(true, at(effect, "before", "email", "$reproit").get("redacted"));
        assertEquals("z", at(effect, "after").get("name"));
        assertFalse(effect.containsKey("extra"));
    }

    @Test
    void canonicalHttpInputLowercasesHeadersAndPreservesRepeatedValues() {
        Map<String, Object> input = BackendTrace.httpInput(
            Map.of("name", "demo"),
            Map.of("project", "p1"),
            Map.of("tag", List.of("a", "b")),
            Map.of("X-Mode", "safe"));
        assertEquals("safe", at(input, "headers").get("x-mode"));
        assertEquals(List.of("a", "b"), at(input, "query").get("tag"));
        assertEquals(Map.of(),
            BackendTrace.httpInput(null, Map.of(), Map.of(), Map.of()));
    }

    @Test
    void selectionsValidateTheirPaths() {
        assertNotNull(BackendTrace.selection("project.id", "projectId", null));
        assertNotNull(BackendTrace.selection("items[].id", "rows[].id", "Widget"));
        assertNull(BackendTrace.selection("1bad", "ok", null));
        assertNull(BackendTrace.selection("ok", "ok", "Bad.Condition"));
    }

    // Full-trace golden: the expected string is the exact canonicalJson the
    // Node SDK produced for the identical trace (sequence stripped, since the
    // counter is process-global).
    @Test
    void traceEventBytesMatchTheNodeSdk() {
        TraceContext context = new TraceContext("trace-g", "alice", 3, "b1", null);
        Map<String, Object> input = new LinkedHashMap<>();
        input.put("item", "widget");
        input.put("password", "hunter22");
        BackendTrace trace = BackendTrace.begin(context, "createOrder",
            new BackendTrace.Options()
                .tenant("org-1")
                .idempotencyKey("retry-secret")
                .input(input));
        trace.effect("write", new BackendTrace.Effect().resource("orders").key("1"));
        Map<String, Object> output = new LinkedHashMap<>();
        output.put("ok", true);
        output.put("apiKey", "sk_live_x");
        trace.finish(output, 201, true, true);
        List<Map<String, Object>> stripped = new ArrayList<>();
        for (Map<String, Object> event : trace.events()) {
            Map<String, Object> copy = new LinkedHashMap<>(event);
            copy.remove("sequence");
            stripped.add(copy);
        }
        String golden = "[{\"actionIndex\":3,\"actor\":\"alice\",\"build\":\"b1\","
            + "\"idempotencyKey\":\"sha256:691a2bdae9040f9fcfe6ff3f\","
            + "\"input\":{\"item\":\"widget\",\"password\":{\"$reproit\":"
            + "{\"length\":8,\"redacted\":true,\"type\":\"string\"}}},\"kind\":\"start\","
            + "\"operation\":\"createOrder\",\"spanId\":\"trace-g:createOrder\","
            + "\"tenant\":\"org-1\",\"traceId\":\"trace-g\"},"
            + "{\"actionIndex\":3,\"actor\":\"alice\",\"build\":\"b1\",\"effect\":\"write\","
            + "\"idempotencyKey\":\"sha256:691a2bdae9040f9fcfe6ff3f\",\"key\":\"1\","
            + "\"kind\":\"effect\",\"operation\":\"createOrder\",\"resource\":\"orders\","
            + "\"spanId\":\"trace-g:createOrder\",\"tenant\":\"org-1\",\"traceId\":\"trace-g\"},"
            + "{\"actionIndex\":3,\"actor\":\"alice\",\"build\":\"b1\",\"effectsComplete\":true,"
            + "\"idempotencyKey\":\"sha256:691a2bdae9040f9fcfe6ff3f\",\"kind\":\"return\","
            + "\"operation\":\"createOrder\",\"output\":{\"apiKey\":{\"$reproit\":"
            + "{\"length\":9,\"redacted\":true,\"type\":\"string\"}},\"ok\":true},"
            + "\"spanId\":\"trace-g:createOrder\",\"status\":201,\"success\":true,"
            + "\"tenant\":\"org-1\",\"traceId\":\"trace-g\"}]";
        assertEquals(golden, Json.canonicalJson(stripped));
    }
}
