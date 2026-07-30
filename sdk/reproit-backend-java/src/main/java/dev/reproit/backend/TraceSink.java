/*
 * Where a framework adapter hands finished traces in production.
 *
 * `Capture` is the shipping implementation (it samples and uploads). The
 * interface exists so an adapter is not welded to the uploader: a fixture or
 * a self-hosted sink can receive the same finished traces without
 * reimplementing the filter.
 */
package dev.reproit.backend;

public interface TraceSink {
    /** A synthesized capture-mode context, replacing the scan-time header. */
    TraceContext context();

    /** Hand over a finished trace. Must never throw into the host app. */
    void record(BackendTrace trace);
}
