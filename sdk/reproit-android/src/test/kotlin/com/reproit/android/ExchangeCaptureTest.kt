package com.reproit.android

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Production exchange capture parity with `sdk/reproit-backend-node/instrument.js`:
 * the same bounds, the same `$reproit` redaction stub, and the same
 * `{protocol, request, response}` shape one replay engine has to serve.
 */
class ExchangeCaptureTest {

  @Test
  fun anOversizedBodyKeepsProvableIdentityOnly() {
    val big = ByteArray(MAX_EXCHANGE_BODY_BYTES + 1) { 'x'.code.toByte() }
    val bounded = boundedBody(big, "text/plain")
    assertEquals(MAX_EXCHANGE_BODY_BYTES + 1, bounded["bodyBytes"])
    assertEquals(true, bounded["truncated"])
    assertNull("an over-budget body must not ship its bytes", bounded["body"])
    assertTrue(Regex("^[0-9a-f]{64}$").matches(bounded["bodySha256"].toString()))
  }

  @Test
  fun bodiesAtOrBelowTheBudgetShipVerbatim() {
    val exact = ByteArray(MAX_EXCHANGE_BODY_BYTES) { 'y'.code.toByte() }
    val bounded = boundedBody(exact, "text/plain")
    assertEquals(MAX_EXCHANGE_BODY_BYTES, bounded["body"].toString().length)
    assertNull(bounded["truncated"])
    // Declared JSON is parsed so structural redaction sees fields, not text.
    val json = boundedBody("""{"a":1}""".toByteArray(), "application/json; charset=utf-8")
    @Suppress("UNCHECKED_CAST") val decoded = json["body"] as Map<String, Any?>
    assertEquals(1.0, decoded["a"])
    // Declared JSON that does not parse falls back to text rather than failing.
    assertEquals("not json", boundedBody("not json".toByteArray(), "application/json")["body"])
    assertTrue(boundedBody(null, "application/json").isEmpty())
    assertTrue(boundedBody(ByteArray(0), "application/json").isEmpty())
  }

  @Test
  fun headersAreLowercasedAndBounded() {
    val many = (1..MAX_EXCHANGE_HEADERS + 5).associate { "X-Header-$it" to "v$it" }
    @Suppress("UNCHECKED_CAST")
    val bounded = boundedHeaders(many)["headers"] as Map<String, Any?>
    assertEquals(MAX_EXCHANGE_HEADERS, bounded.size)
    assertTrue(bounded.keys.all { it == it.lowercase() })
    assertTrue(boundedHeaders(emptyMap()).isEmpty())
  }

  @Test
  fun redactionInsideExchangeBodiesUsesNodesPlaceholder() {
    val exchange =
      captureHttpExchange(
        method = "post",
        url = "https://api.example.com/login",
        requestHeaders = mapOf("Content-Type" to "application/json", "Authorization" to "Bearer t"),
        requestBody = """{"email":"a@b.c","note":"keep"}""".toByteArray(),
        status = 500,
        responseHeaders = mapOf("Content-Type" to "application/json"),
        responseBody = """{"apiKey":"sk-live-secret","ok":false}""".toByteArray(),
      )
    val encoded = Json.encode(exchange)
    assertFalse("a secret value must never leave the process", encoded.contains("sk-live-secret"))
    assertFalse(encoded.contains("a@b.c"))
    assertFalse(encoded.contains("Bearer t"))
    assertTrue("ordinary fields survive", encoded.contains("keep"))
    // The replay matcher keys on the `$reproit` marker to wildcard a position;
    // a plain `<reproit:...>` string would be compared literally and diverge.
    assertTrue(encoded.contains("\"\$reproit\""))

    @Suppress("UNCHECKED_CAST") val request = exchange["request"] as Map<String, Any?>
    @Suppress("UNCHECKED_CAST") val response = exchange["response"] as Map<String, Any?>
    assertEquals("http", exchange["protocol"])
    assertEquals("POST", request["method"])
    assertEquals("https://api.example.com/login", request["url"])
    assertEquals(500, response["status"])
    @Suppress("UNCHECKED_CAST") val body = response["body"] as Map<String, Any?>
    @Suppress("UNCHECKED_CAST") val stub = body["apiKey"] as Map<String, Any?>
    @Suppress("UNCHECKED_CAST") val meta = stub["\$reproit"] as Map<String, Any?>
    assertEquals(true, meta["redacted"])
    assertEquals("string", meta["type"])
    assertEquals("sk-live-secret".length, meta["length"])
    assertEquals(false, body["ok"])
  }

  @Test
  fun captureSecretVocabularyMatchesNode() {
    // Node folds every non-alphanumeric out of the key before matching.
    for (name in
      listOf(
        "password",
        "api-key",
        "API_KEY",
        "Access Key",
        "x-idempotency-key",
        "Authorization",
        "signingKey",
      )) {
      assertTrue("$name must be treated as a secret", captureSecretField(name))
    }
    for (name in listOf("keyboardLayout", "key", "username", "count")) {
      assertFalse("$name is ordinary", captureSecretField(name))
    }
    // Idempotency keys are the one addition over the runner vocabulary, which
    // stays frozen so runner output is byte-unchanged.
    assertTrue(captureSecretField("idempotencyKey"))
    assertFalse(causalSecretField("idempotencyKey"))
  }

  @Test
  fun theRunnerRedactionVocabularyIsUnchanged() {
    val raw = linkedMapOf<String, Any?>("apiKey" to "raw-api", "keyboardLayout" to "dvorak")
    @Suppress("UNCHECKED_CAST") val safe = redactCausalValue(raw) as Map<String, Any?>
    assertEquals("<reproit:string:length=7>", safe["apiKey"])
    assertEquals("dvorak", safe["keyboardLayout"])
  }

  @Test
  fun theDeterminismEnvelopeCarriesIdentityAndSeed() {
    val envelope =
      determinismEnvelope(
        observedAtMs = 1753747200000L,
        osRelease = "14",
        arch = "arm64-v8a",
        replaySeed = "c0ffee00c0ffee00",
        imageDigest = null,
      )
    assertEquals(1753747200000L, envelope["observedAtMs"])
    assertEquals("android", envelope["runtime"])
    assertEquals("14", envelope["os"])
    assertEquals("arm64-v8a", envelope["arch"])
    assertEquals("c0ffee00c0ffee00", envelope["replaySeed"])
    assertNotNull("the capture timezone pins the replay clock", envelope["tz"])
    assertNull("an absent image digest is omitted, never guessed", envelope["imageDigest"])
    assertEquals(16, replaySeedHex { 1L }.length)
  }

  @Test
  fun `a null in a captured body survives to the wire`() {
    // Found on a real emulator run: the upstream answered {"prices": null},
    // the crash was CAUSED by that null, and the encoder dropped the key, so
    // the capsule described a response the upstream never sent. A capture that
    // loses a null reproduces a different error than the one that happened.
    val body = """{"prices":null,"symbol":"ACME"}""".toByteArray()
    val bounded = boundedBody(body, "application/json")
    val encoded = Json.encode(bounded["body"])
    assertTrue("the null key must survive: $encoded", encoded.contains("\"prices\":null"))
    assertTrue("sibling values are unaffected: $encoded", encoded.contains("\"symbol\":\"ACME\""))
  }

  @Test
  fun `an absent optional event field is still omitted`() {
    // The sentinel must NOT change the event model's wire: optional fields
    // stay off it, matching the other SDKs and the golden byte pins.
    val encoded = Json.encode(linkedMapOf("from" to null, "to" to "abc"))
    assertEquals("""{"to":"abc"}""", encoded)
  }
}
