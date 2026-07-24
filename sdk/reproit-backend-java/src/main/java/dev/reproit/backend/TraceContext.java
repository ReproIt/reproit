/*
 * Trace correlation context: parsed from trusted `x-reproit-*` request headers
 * at scan time (BackendTrace.traceContextFromHeaders) or synthesized by
 * capture mode (Capture.context). All fields except traceId are optional.
 */
package dev.reproit.backend;

public record TraceContext(
    String traceId, String actor, long actionIndex, String build, String configContract) {}
