/*
 * Trace correlation context: parsed from trusted `x-reproit-*` request headers
 * at scan time (BackendTrace.traceContextFromHeaders) or synthesized by
 * capture mode (Capture.context). All fields except traceId are optional.
 */
package dev.reproit.backend;

public record TraceContext(
    String traceId,
    String actor,
    long actionIndex,
    String build,
    String configContract,
    boolean captureEnvelope) {

    /**
     * Scan-time contexts never stamp the determinism envelope; capture mode
     * opts in through the six-argument form.
     */
    public TraceContext(
            String traceId, String actor, long actionIndex, String build, String configContract) {
        this(traceId, actor, actionIndex, build, configContract, false);
    }
}
