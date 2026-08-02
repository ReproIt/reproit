// Agent oracle API parity with the Node reference (capture.test.js): unknown
// ids are rejected, a marked operation is always captured, and the failure
// observation carries the marked id as an authored contract violation.
package dev.reproit.backend;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertThrows;

import java.util.ArrayList;
import java.util.List;
import java.util.Map;
import org.junit.jupiter.api.Test;

class AgentOracleTest {
    private static Capture capture() {
        return Capture.create(new Capture.Config()
            .endpoint("http://127.0.0.1:9/v1/capture-batches")
            .apiKey("sk")
            .appId("app"));
    }

    @Test
    void oracleMarkersRideTheTraceAndRejectUnknownIds() {
        Capture capture = capture();
        BackendTrace trace = BackendTrace.begin(
            capture.context(), "POST /assist", new BackendTrace.Options());
        assertThrows(TraceError.class, () -> trace.oracle("made-up-oracle", null));
        trace.oracle(Capture.AGENT_GUARDRAIL_ORACLE, Map.of("tool", "delete_order"));
        trace.finish(Map.of("error", "guardrail"), 500, false, true);
        assertEquals(
            Capture.AGENT_GUARDRAIL_ORACLE, Capture.markedOracle(trace.events()));
    }

    @Test
    void aMarkedOperationIsCapturedEvenWithoutA5xx() {
        Capture capture = capture();
        BackendTrace trace = BackendTrace.begin(
            capture.context(), "POST /assist", new BackendTrace.Options());
        trace.oracle(Capture.AGENT_LOOP_BOUND_ORACLE, Map.of("iterations", 9L, "bound", 4L));
        trace.finish(Map.of("note", "gave up"), 200, true, true);
        capture.record(trace);
        assertEquals(1, capture.stats().capturedOperations());
    }

    @Test
    void aMarkedFailureObservationCarriesTheAgentOracleId() {
        Capture capture = capture();
        BackendTrace trace = BackendTrace.begin(
            capture.context(), "POST /assist", new BackendTrace.Options());
        trace.oracle(Capture.AGENT_GUARDRAIL_ORACLE, Map.of("tool", "delete_order"));
        trace.finish(Map.of("error", "guardrail"), 500, false, true);
        Map<String, Object> batch = capture.buildBatch(List.of(new Capture.Operation(
            "POST /assist", 500, new ArrayList<>(trace.events()))));
        @SuppressWarnings("unchecked")
        List<Map<String, Object>> events = (List<Map<String, Object>>) batch.get("events");
        @SuppressWarnings("unchecked")
        Map<String, Object> observation =
            (Map<String, Object>) events.get(events.size() - 1).get("event");
        assertEquals("observation", observation.get("kind"));
        @SuppressWarnings("unchecked")
        Map<String, Object> failure = (Map<String, Object>) observation.get("failure");
        assertEquals(Capture.AGENT_GUARDRAIL_ORACLE + ":POST /assist", failure.get("signature"));
        assertEquals("contract-violation", failure.get("observation"));
        assertEquals(
            "agent oracle " + Capture.AGENT_GUARDRAIL_ORACLE + " fired on POST /assist",
            failure.get("summary"));
    }

    @Test
    void theCapturePayloadOracleIsTheMarkedId() {
        Capture capture = capture();
        BackendTrace trace = BackendTrace.begin(
            capture.context(), "POST /assist", new BackendTrace.Options());
        trace.oracle(Capture.AGENT_RESPONSE_ORACLE, null);
        trace.finish(null, 200, true, true);
        Capture.Payload payload = Capture.capturePayload(new Capture.Operation(
            "POST /assist", null, new ArrayList<>(trace.events())));
        assertEquals(Capture.AGENT_RESPONSE_ORACLE, payload.value().get("oracle"));
    }
}
