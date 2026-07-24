/*
 * Rejection of invalid adapter input, mirroring the Rust adapter's error enum.
 * Codes: InvalidOperation, AlreadyFinished, TooManyEvents, HeaderTooLarge.
 */
package dev.reproit.backend;

public final class TraceError extends RuntimeException {
    public final String code;

    TraceError(String code) {
        super("reproit trace rejected input: " + code);
        this.code = code;
    }
}
