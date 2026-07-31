package com.reproit.android

/**
 * Drive the real [CausalHttp] replay path into an unmatched call on a device.
 *
 * The capsule and the live call both come from the shared vectors
 * (`sdk/capture-behavior-v1.json`, vocabularies.divergenceMarkers.parityScenario)
 * so all three mobile platforms are asked the same question. The SDK writes
 * `REPROIT:DIVERGENCE` to stderr itself; this probe only reports the frozen
 * runner contract, which reaches the caller as a thrown message rather than a
 * stream, on stdout.
 */
fun main() {
  val http = CausalHttp()
  try {
    http.request(System.getenv("PROBE_URL"), "GET")
    println("PROBE:NOMISS")
  } catch (error: Throwable) {
    println("PROBE:MISS ${error.message}")
  }
}
