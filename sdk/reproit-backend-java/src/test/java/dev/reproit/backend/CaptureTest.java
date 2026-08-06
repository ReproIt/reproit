// Capture-mode parity tests against sdk/reproit-backend-rs/src/capture.rs,
// ported from the Node and Python SDK test suites. Batches round-trip through
// EventBatchV1, the Java port of the protocol mirror in
// sdk/test/event_batch_v1.js.
package dev.reproit.backend;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import org.junit.jupiter.api.AfterEach;
import org.junit.jupiter.api.BeforeEach;
import org.junit.jupiter.api.Test;

class CaptureTest {
    // Keep the suite hermetic against the runner. Capture.resolveCommit falls
    // back to REPROIT_COMMIT then GITHUB_SHA, which is correct behavior, but a
    // GitHub runner always sets GITHUB_SHA and a laptop never does, so the
    // exact-shape deployment assertions below passed locally and failed in CI
    // the first time this SDK gated. The fallback itself is proven on purpose
    // by aCiRunnerSuppliesTheCommitTheConfigOmits.
    private Map<String, String> savedEnvironment;

    @BeforeEach
    void clearAmbientCodeIdentity() {
        savedEnvironment = Capture.environment;
        Capture.environment = Map.of();
    }

    @AfterEach
    void restoreAmbientCodeIdentity() {
        Capture.environment = savedEnvironment;
    }

    @Test
    void aCiRunnerSuppliesTheCommitTheConfigOmits() {
        String sha = "f857cb7740a5f857cb7740a5f857cb7740a5f857";
        Capture.environment = Map.of("GITHUB_SHA", sha);
        assertEquals(
            Map.of("version", "1.2.3", "commit", sha),
            batchFor(500, false).get("deployment"));
    }
    private static Capture capture(String build) {
        return Capture.create(new Capture.Config()
            .endpoint("http://127.0.0.1:9/v1/capture-batches")
            .apiKey("sk")
            .appId("app-demo")
            .build(build));
    }

    private static BackendTrace finishedTrace(Capture capture, int status, boolean success) {
        BackendTrace trace = BackendTrace.begin(capture.context(), "createOrder",
            new BackendTrace.Options()
                .input(Map.of("body", Map.of("item", "widget", "qty", 2L))));
        trace.effect("read", new BackendTrace.Effect()
            .resource("inventory")
            .key("widget")
            .exchange(Map.of(
                "request", Map.of("key", "widget"),
                "response", Map.of("available", true))));
        trace.finish(Map.of("error", "boom"), status, success, true);
        return trace;
    }

    private static Map<String, Object> batchFor(int status, boolean success) {
        Capture capture = capture("1.2.3");
        BackendTrace trace = finishedTrace(capture, status, success);
        return capture.buildBatch(List.of(new Capture.Operation(
            "createOrder", status, new ArrayList<>(trace.events()))));
    }

    @SuppressWarnings("unchecked")
    private static Map<String, Object> at(Object value, String... path) {
        Map<String, Object> current = (Map<String, Object>) value;
        for (String key : path) {
            current = (Map<String, Object>) current.get(key);
        }
        return current;
    }

    @SuppressWarnings("unchecked")
    private static List<Map<String, Object>> events(Map<String, Object> batch) {
        return (List<Map<String, Object>>) batch.get("events");
    }

    @Test
    void serverErrorBatchUsesUniversalCausalContract() {
        Map<String, Object> batch = batchFor(500, false);
        assertEquals("app-demo", batch.get("projectId"));
        assertEquals(Map.of("version", "1.2.3"), batch.get("deployment"));
        assertEquals(7, events(batch).size());
        // The determinism envelope rides as a named checkpoint after the trigger.
        Map<String, Object> envelope = at(events(batch).get(2), "event");
        assertEquals("checkpoint", envelope.get("kind"));
        assertEquals("determinism-envelope", envelope.get("name"));
        assertEquals(
            16, String.valueOf(at(envelope, "attributes").get("replaySeed")).length());
        Map<String, Object> finding = at(events(batch).get(6), "event");
        assertEquals("observation", finding.get("kind"));
        assertEquals(
            Capture.SERVER_ERROR_ORACLE + ":createOrder",
            at(finding, "failure").get("signature"));
        // Redaction happened before anything left the process boundary.
        Map<String, Object> trigger = at(events(batch).get(1), "event", "value", "value");
        assertEquals("widget", at(trigger, "body").get("item"));
        // The raw return event is nested like the raw effects, under a
        // subject that names the carrier for the protocol projection.
        Map<String, Object> carrier = at(events(batch).get(4), "event");
        assertEquals("effect", carrier.get("kind"));
        assertEquals("operation-return", carrier.get("subject"));
        Map<String, Object> rawReturn = at(carrier, "value", "value");
        assertEquals("return", rawReturn.get("kind"));
        assertEquals(500, rawReturn.get("status"));
    }

    @Test
    void healthyOperationsShipCausalEventsWithoutObservation() {
        Map<String, Object> batch = batchFor(201, true);
        assertEquals(6, events(batch).size());
        for (Map<String, Object> event : events(batch)) {
            assertFalse("observation".equals(at(event, "event").get("kind")));
        }
    }

    @Test
    void oversizedCapturesDropTrailingEffectsFirst() {
        Capture source = capture(null);
        List<Map<String, Object>> events =
            new ArrayList<>(finishedTrace(source, 500, false).events());
        Map<String, Object> filler = new LinkedHashMap<>();
        filler.put("kind", "effect");
        filler.put("effect", "write");
        filler.put("resource", "x".repeat(Capture.MAX_CAPTURE_JSON_BYTES));
        events.add(2, filler);
        Capture.Payload payload = Capture.capturePayload(
            new Capture.Operation("createOrder", 500, events));
        assertEquals(1, payload.droppedEffects());
        List<?> kept = (List<?>) payload.value().get("events");
        assertEquals(3, kept.size());
        Map<String, Object> effect = (Map<String, Object>) kept.get(1);
        assertEquals("effect", effect.get("kind"));
        assertEquals("inventory", effect.get("resource"));
    }

    @Test
    void aCaptureThatCannotFitStartPlusReturnIsOmitted() {
        Map<String, Object> start = new LinkedHashMap<>();
        start.put("kind", "start");
        start.put("operation", "op");
        start.put("input", Map.of("blob", "x".repeat(Capture.MAX_CAPTURE_JSON_BYTES)));
        Map<String, Object> returned = new LinkedHashMap<>();
        returned.put("kind", "return");
        returned.put("status", 500);
        returned.put("success", false);
        Capture.Operation operation = new Capture.Operation("op", 500, List.of(start, returned));
        assertNull(Capture.capturePayload(operation));
    }

    @Test
    void unusableConfigsDisableCaptureInsteadOfFailing() {
        assertNull(Capture.create(null));
        assertNull(Capture.create(
            new Capture.Config().endpoint("").apiKey("sk").appId("app")));
        assertNull(Capture.create(
            new Capture.Config().endpoint("http://c").apiKey("").appId("app")));
        assertNull(Capture.create(
            new Capture.Config().endpoint("http://c").apiKey("sk").appId("bad app")));
        assertNull(Capture.create(new Capture.Config()
            .endpoint("http://c").apiKey("sk").appId("app").build("bad build")));
    }

    @Test
    void recordSamplesFailuresOnlyByDefault() {
        Capture capture = capture(null);
        BackendTrace open = BackendTrace.begin(capture.context(), "op", null);
        capture.record(open);
        BackendTrace healthy = BackendTrace.begin(capture.context(), "op", null);
        healthy.finish(null, 200, true, true);
        capture.record(healthy);
        assertEquals(0, capture.stats().capturedOperations());
        BackendTrace failed = BackendTrace.begin(capture.context(), "op", null);
        failed.finish(null, 200, false, true);
        capture.record(failed);
        assertEquals(0, capture.stats().capturedOperations());
        assertTrue(capture.flush(10000));
        Capture.Stats stats = capture.stats();
        assertEquals(0, stats.failedBatches());
        assertEquals(0, stats.droppedOperations());
    }

    @Test
    void recordQueuesOnlyAPortableServerFailure() {
        Capture capture = capture(null);
        capture.record(finishedTrace(capture, 500, false));
        assertEquals(1, capture.stats().capturedOperations());
    }
}
