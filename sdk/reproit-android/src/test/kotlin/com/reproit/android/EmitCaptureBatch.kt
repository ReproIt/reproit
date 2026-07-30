package com.reproit.android

/**
 * Print one real failure capture batch to stdout so the host test runner can
 * pipe it through the Rust protocol validator
 * (`cargo run -q -p reproit-protocol --bin capture-validate`). Keeping the
 * emitter in the test source means the validated bytes are the same bytes the
 * SDK builds, not a hand-written fixture.
 */
object EmitCaptureBatch {
  @JvmStatic
  fun main(args: Array<String>) {
    val exchange =
      CapturedExchange(
        subject = "pricing.internal",
        exchange =
          captureHttpExchange(
            method = "GET",
            url = "https://pricing.internal/prices?tier=gold",
            requestHeaders = mapOf("Accept" to "application/json", "Authorization" to "Bearer t"),
            requestBody = null,
            status = 200,
            responseHeaders = mapOf("Content-Type" to "application/json"),
            responseBody = """{"prices":null,"apiKey":"sk-live-secret"}""".toByteArray(),
          ),
        atMs = 1753747200000L,
        monotonicNs = 4_000_000L,
      )
    val batch =
      buildFailureCaptureBatch(
        appId = "com.example.app",
        sessionId = "session-android-1753747200000",
        operation = "crash:NullPointerException",
        triggerAction = "tap:key:checkout",
        signature = "crash:java.lang.NullPointerException",
        summary = "java.lang.NullPointerException: prices was null",
        observationPoint = "com.example.Checkout.total(Checkout.kt:42)",
        exchanges = listOf(exchange),
        envelope =
          determinismEnvelope(1753747200000L, "14", "arm64-v8a", "c0ffee00c0ffee00", null),
        buildVersion = "1.2.3",
        buildCommit = "abc123",
        observedAtIso = "2026-07-30T12:00:00Z",
        batchSequence = 1,
        observedAtMs = 1753747200000L,
      )
    if (batch == null) {
      System.err.println("capture batch refused to build")
      kotlin.system.exitProcess(1)
    }
    println(Json.encode(batch))
  }
}
