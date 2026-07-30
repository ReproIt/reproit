package com.reproit.android

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The emitted `capture-batch-v1` for a failure occurrence that carries recorded
 * exchanges. Shape parity with `sdk/reproit-backend-node/capture.js`; the wire
 * itself is proven against the Rust protocol validator by
 * `emit_capture_batch.kt` in the host test runner.
 */
class CaptureBatchTest {

  private fun exchange(host: String, status: Int, monotonicNs: Long) =
    CapturedExchange(
      subject = host,
      exchange =
        captureHttpExchange(
          method = "GET",
          url = "https://$host/prices",
          requestHeaders = mapOf("Accept" to "application/json"),
          requestBody = null,
          status = status,
          responseHeaders = mapOf("Content-Type" to "application/json"),
          responseBody = """{"prices":null}""".toByteArray(),
        ),
      atMs = 1753747200000L,
      monotonicNs = monotonicNs,
    )

  private fun batch(exchanges: List<CapturedExchange>): Map<String, Any?>? =
    buildFailureCaptureBatch(
      appId = "com.example.app",
      sessionId = "session-android-1753747200000",
      operation = "crash:NullPointerException",
      triggerAction = "tap:key:checkout",
      signature = "crash:java.lang.NullPointerException",
      summary = "java.lang.NullPointerException: prices was null",
      observationPoint = "com.example.Checkout.total(Checkout.kt:42)",
      exchanges = exchanges,
      envelope =
        determinismEnvelope(1753747200000L, "14", "arm64-v8a", "c0ffee00c0ffee00", null),
      buildVersion = "1.2.3",
      buildCommit = "abc123",
      observedAtIso = "2026-07-30T12:00:00Z",
      batchSequence = 1,
      observedAtMs = 1753747200000L,
    )

  @Test
  fun theBatchCarriesTheCausalSequenceAndEveryExchange() {
    val built = assertNotNull(batch(listOf(exchange("pricing.internal", 200, 4_000_000L))))
    val value = batch(listOf(exchange("pricing.internal", 200, 4_000_000L)))!!
    assertEquals(1, value["version"])
    assertEquals("com.example.app", value["projectId"])
    @Suppress("UNCHECKED_CAST") val emitter = value["emitter"] as Map<String, Any?>
    assertEquals("mobile-android", emitter["id"])
    assertEquals("runtime-sdk", emitter["kind"])
    assertEquals("android", emitter["runtime"])
    @Suppress("UNCHECKED_CAST") val deployment = value["deployment"] as Map<String, Any?>
    assertEquals("1.2.3", deployment["version"])
    assertEquals("abc123", deployment["commit"])

    @Suppress("UNCHECKED_CAST") val events = value["events"] as List<Map<String, Any?>>
    val kinds =
      events.map {
        @Suppress("UNCHECKED_CAST") val event = it["event"] as Map<String, Any?>
        event["kind"]
      }
    assertEquals(
      listOf(
        "operation-start",
        "trigger",
        "checkpoint",
        "dependency",
        "operation-end",
        "observation",
      ),
      kinds,
    )
    // Dense sequence, and each event names its causal parent.
    events.forEachIndexed { index, event -> assertEquals(index + 1, event["sequence"]) }
    for (index in 1 until events.size) {
      @Suppress("UNCHECKED_CAST") val parents = events[index]["causalParentIds"] as List<String>
      assertEquals(listOf(events[index - 1]["id"]), parents)
    }
    @Suppress("UNCHECKED_CAST") val first = events[0]["causalParentIds"] as List<String>
    assertTrue("the first event has no parent", first.isEmpty())
    assertNotNull(built)
  }

  @Test
  fun theEnvelopeRidesAsANamedCheckpointAndTheExchangeIsReplayable() {
    val value = batch(listOf(exchange("pricing.internal", 200, 4_000_000L)))!!
    @Suppress("UNCHECKED_CAST") val events = value["events"] as List<Map<String, Any?>>
    @Suppress("UNCHECKED_CAST") val checkpoint = events[2]["event"] as Map<String, Any?>
    assertEquals("determinism-envelope", checkpoint["name"])
    @Suppress("UNCHECKED_CAST") val attributes = checkpoint["attributes"] as Map<String, Any?>
    assertEquals("c0ffee00c0ffee00", attributes["replaySeed"])

    @Suppress("UNCHECKED_CAST") val dependency = events[3]["event"] as Map<String, Any?>
    assertEquals("service", dependency["system"])
    assertEquals("call", dependency["operation"])
    assertEquals("pricing.internal", dependency["subject"])
    @Suppress("UNCHECKED_CAST") val captured = dependency["value"] as Map<String, Any?>
    assertEquals("replayable", captured["representation"])
    assertEquals("redacted-at-source", captured["redaction"])
    @Suppress("UNCHECKED_CAST") val payload = captured["value"] as Map<String, Any?>
    assertEquals("http", payload["protocol"])
    // Real monotonic offsets, not the ordinal fallback.
    assertEquals(4_000_000L, events[3]["monotonicNs"])
  }

  @Test
  fun theNetworkCapabilityIsClaimedOnlyWhenExchangesExist() {
    val withExchanges = batch(listOf(exchange("pricing.internal", 200, 1L)))!!
    @Suppress("UNCHECKED_CAST")
    val claimed = withExchanges["capabilities"] as List<Map<String, Any?>>
    assertTrue(claimed.any { it["capability"] == "network" && it["completeness"] == "complete" })

    val without = batch(emptyList())!!
    @Suppress("UNCHECKED_CAST") val bare = without["capabilities"] as List<Map<String, Any?>>
    assertFalse(
      "an empty capture must never claim complete network coverage",
      bare.any { it["capability"] == "network" },
    )
  }

  @Test
  fun anUnusableProjectIdentifierRefusesToBuildABatch() {
    // The ingest protocol rejects these, so the SDK keeps the legacy path
    // rather than emitting something the server will drop.
    val bad =
      buildFailureCaptureBatch(
        appId = "not a token",
        sessionId = "session-1",
        operation = "crash",
        triggerAction = "load",
        signature = "crash:X",
        summary = "boom",
        observationPoint = "X",
        exchanges = emptyList(),
        envelope = determinismEnvelope(1L, "14", "arm64", "00ff00ff00ff00ff", null),
        buildVersion = null,
        buildCommit = null,
        observedAtIso = "2026-07-30T12:00:00Z",
        batchSequence = 1,
        observedAtMs = 1L,
      )
    assertNull(bad)
    assertTrue(validToken("com.example.app"))
    assertFalse(validToken("not a token"))
  }

  @Test
  fun anAbsentBuildIdentityOmitsDeploymentEntirely() {
    val value =
      buildFailureCaptureBatch(
        appId = "com.example.app",
        sessionId = "session-1",
        operation = "crash",
        triggerAction = "load",
        signature = "crash:X",
        summary = "boom",
        observationPoint = "X",
        exchanges = emptyList(),
        envelope = determinismEnvelope(1L, "14", "arm64", "00ff00ff00ff00ff", null),
        buildVersion = null,
        buildCommit = null,
        observedAtIso = "2026-07-30T12:00:00Z",
        batchSequence = 1,
        observedAtMs = 1L,
      )!!
    assertNull(value["deployment"])
  }
}
