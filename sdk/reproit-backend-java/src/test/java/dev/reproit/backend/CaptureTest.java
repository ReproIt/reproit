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
import org.junit.jupiter.api.Test;

class CaptureTest {
    private static Capture capture(String build) {
        return Capture.create(new Capture.Config()
            .endpoint("http://127.0.0.1:9/v1/events")
            .apiKey("sk")
            .appId("app-demo")
            .build(build));
    }

    private static BackendTrace finishedTrace(Capture capture, int status, boolean success) {
        BackendTrace trace = BackendTrace.begin(capture.context(), "createOrder",
            new BackendTrace.Options()
                .input(Map.of("body", Map.of("item", "widget", "qty", 2L))));
        trace.effect("read", new BackendTrace.Effect().resource("inventory").key("widget"));
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
    private static List<Map<String, Object>> frames(Map<String, Object> batch) {
        return (List<Map<String, Object>>) batch.get("frames");
    }

    @Test
    void serverErrorBatchIsAValidTaggedEventBatch() {
        Map<String, Object> batch = batchFor(500, false);
        EventBatchV1.validateEventBatch(batch);
        assertEquals(Map.of("version", "1.2.3"), batch.get("deployment"));
        assertEquals(4, frames(batch).size());
        Map<String, Object> finding = at(frames(batch).get(3), "event");
        assertEquals("finding", finding.get("kind"));
        assertEquals(Capture.SERVER_ERROR_ORACLE, at(finding, "identity").get("oracle"));
        Map<String, Object> replay = at(finding, "context", "reproitCapture");
        assertEquals(Capture.CAPTURE_FORMAT, replay.get("format"));
        assertEquals("createOrder", replay.get("operation"));
        assertEquals(3, ((List<?>) replay.get("events")).size());
        // Redaction happened before anything left the process boundary.
        Map<String, Object> start = (Map<String, Object>) ((List<?>) replay.get("events")).get(0);
        assertEquals("widget", at(start, "input", "body").get("item"));
    }

    @Test
    void healthyOperationsShipBackendFramesWithoutAFinding() {
        Map<String, Object> batch = batchFor(201, true);
        EventBatchV1.validateEventBatch(batch);
        assertEquals(3, frames(batch).size());
        for (Map<String, Object> frame : frames(batch)) {
            assertEquals("backend", at(frame, "event").get("kind"));
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
        Map<String, Object> batch = capture(null).buildBatch(List.of(operation));
        List<Map<String, Object>> frames = frames(batch);
        Map<String, Object> finding = at(frames.get(frames.size() - 1), "event");
        assertEquals(true, at(finding, "context").get("captureOmitted"));
        assertFalse(at(finding, "context").containsKey("reproitCapture"));
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
        assertEquals(1, capture.stats().capturedOperations());
        assertTrue(capture.flush(10000));
        Capture.Stats stats = capture.stats();
        // 127.0.0.1:9 refuses connections: the batch fails, the op is dropped.
        assertEquals(1, stats.failedBatches());
        assertEquals(1, stats.droppedOperations());
    }
}
