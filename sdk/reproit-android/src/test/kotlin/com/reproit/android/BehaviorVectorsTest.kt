package com.reproit.android

import java.io.File
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Shared behavioral conformance vectors (`sdk/capture-behavior-v1.json`).
 *
 * Twenty SDKs hand implement one capture contract, so a defect otherwise has to
 * be found twenty times. Every group here was written against a defect that
 * actually shipped, and Android is the reason two of them exist:
 *
 *   bounds        an over-budget body must keep only its byte count and sha256,
 *                 measured in ENCODED BYTES rather than characters
 *   headers       the 32 header cap is taken over NAME SORTED order. Go recorded
 *                 a different subset each run by capping a randomized map first;
 *                 this SDK capped in arrival order, which is the same defect with
 *                 a stable disguise
 *   redaction     the `$reproit` stub keeps type and length so the replay matcher
 *                 can wildcard the value, and redaction is STRUCTURE PRESERVING:
 *                 an explicit null stays a null. This SDK once recorded
 *                 {"symbol":"ACME"} where production sent {"prices":null} and
 *                 replay reproduced a DIFFERENT error than production
 *   triggerTokens iOS and React Native both shipped `user-action`, which is not
 *                 in the protocol vocabulary
 *   vocabularies  the structured divergence marker is emitted ALONGSIDE the frozen
 *                 CAPSULE:MISS runner contract, never instead of it
 *
 * Comparisons run over canonical JSON (recursively key sorted, nulls kept as
 * data) so a map ordering difference is not read as a behavioral one, and so an
 * absent key is distinguishable from a key holding null.
 */
class BehaviorVectorsTest {

  private val vectors: Map<String, Any?> = loadVectors()

  @Suppress("UNCHECKED_CAST")
  private fun group(name: String): Map<String, Any?> = vectors[name] as Map<String, Any?>

  @Suppress("UNCHECKED_CAST")
  private fun cases(groupName: String, key: String = "cases"): List<Map<String, Any?>> =
    group(groupName)[key] as List<Map<String, Any?>>

  @Test
  fun constantsMatchTheSharedVectors() {
    val constants = group("constants")
    assertEquals(number(constants["maxExchangeBodyBytes"]), MAX_EXCHANGE_BODY_BYTES)
    assertEquals(number(constants["maxExchangeHeaders"]), MAX_EXCHANGE_HEADERS)
  }

  @Test
  fun boundsVectors() {
    for (case in cases("bounds")) {
      @Suppress("UNCHECKED_CAST") val input = case["input"] as Map<String, Any?>
      val actual = boundedBody(bodyOf(input), input["contentType"] as String?)
      assertEquals(
        "bounds case ${case["name"]}",
        canonical(expandRepeats(case["expect"])),
        canonical(actual),
      )
    }
  }

  @Test
  fun headerVectors() {
    for (case in cases("headers")) {
      @Suppress("UNCHECKED_CAST") val expect = case["expect"] as Map<String, Any?>
      val generated = case["inputGenerated"]
      if (generated == null) {
        @Suppress("UNCHECKED_CAST") val input = case["input"] as Map<String, Any?>
        @Suppress("UNCHECKED_CAST") val headers = input["headers"] as Map<String, String>
        val actual = boundedHeaders(headers)
        val wanted = if (expect.isEmpty()) emptyMap<String, Any?>() else expect
        assertEquals("headers case ${case["name"]}", canonical(wanted), canonical(actual))
        continue
      }
      // Fed in a non-sorted permutation on purpose: an implementation that caps
      // in arrival order keeps x-h00..x-h39 minus a scattered 8, not x-h00..x-h31.
      @Suppress("UNCHECKED_CAST") val spec = generated as Map<String, Any?>
      val total = number(spec["headerCount"])
      val scrambled = LinkedHashMap<String, String>()
      for (step in 0 until total) {
        val index = step * 17 % total
        scrambled[String.format(spec["namePattern"] as String, index).uppercase()] =
          spec["value"] as String
      }
      @Suppress("UNCHECKED_CAST")
      val actual = boundedHeaders(scrambled)["headers"] as Map<String, Any?>
      val names = actual.keys.toList()
      assertEquals("headers case ${case["name"]} count", number(expect["headerCount"]), names.size)
      assertEquals(
        "the cap must be taken over sorted names, not arrival order",
        expect["firstName"],
        names.first(),
      )
      assertEquals(
        "the cap must be taken over sorted names, not arrival order",
        expect["lastName"],
        names.last(),
      )
    }
  }

  @Test
  fun redactionTypeVectors() {
    assertVectorPairs(cases("redaction", "typeCases"), "type")
  }

  @Test
  fun redactionNestingVectors() {
    assertVectorPairs(cases("redaction", "nestingCases"), "nesting")
  }

  @Test
  fun redactionStructureVectors() {
    assertVectorPairs(cases("redaction", "structureCases"), "structure")
  }

  @Test
  fun redactionFoldingVectors() {
    for (case in cases("redaction", "foldingCases")) {
      val field = case["field"] as String
      val secret = case["secret"] as Boolean
      @Suppress("UNCHECKED_CAST")
      val out = redactCaptureValue(mapOf(field to "value")) as Map<String, Any?>
      val redacted = (out[field] as? Map<*, *>)?.containsKey("\$reproit") == true
      assertEquals("$field should${if (secret) "" else " not"} fold to a secret", secret, redacted)
    }
  }

  /**
   * The capture list is the fourteen part one. The runner wire deliberately
   * carries thirteen, and the difference is asserted so it cannot be closed by
   * accident in either direction.
   */
  @Test
  fun theTwoSecretVocabulariesStayDistinct() {
    @Suppress("UNCHECKED_CAST")
    val captureParts = group("redaction")["secretParts"] as List<String>
    @Suppress("UNCHECKED_CAST")
    val causalParts = group("causalRedaction")["secretParts"] as List<String>
    for (part in captureParts) assertTrue(part, captureSecretField(part))
    for (part in causalParts) assertTrue(part, causalSecretField(part))
    assertTrue("idempotencyKey is a capture secret", captureSecretField("idempotency-key"))
    assertFalse("and deliberately not a runner one", causalSecretField("idempotency-key"))
  }

  @Test
  fun theTriggerTokenThisSdkEmitsIsInTheProtocolVocabulary() {
    val tokens = group("triggerTokens")
    @Suppress("UNCHECKED_CAST") val byKind = tokens["bySdkKind"] as Map<String, String>
    @Suppress("UNCHECKED_CAST") val allowed = tokens["allowed"] as List<String>
    @Suppress("UNCHECKED_CAST") val rejected = tokens["rejected"] as List<String>
    val token = byKind["mobile"]!!
    assertTrue(allowed.contains(token))
    val text = sourceFile("CaptureBatch.kt").readText()
    assertTrue("CaptureBatch.kt must emit $token", text.contains("\"$token\""))
    for (bad in rejected) assertFalse("must not emit $bad", text.contains("\"$bad\""))
  }

  /**
   * Gap 3 of the invariant ledger, at unit scale. The cross-platform proof that
   * Android, iOS and React Native agree is
   * `validation/mobile/divergence-parity/run.sh`, which runs this same path on a
   * real emulator; this case keeps the two markers from drifting apart in a suite
   * that needs no device.
   */
  @Test
  fun bothDivergenceMarkersAreEmittedTogether() {
    @Suppress("UNCHECKED_CAST")
    val markers = group("vocabularies")["divergenceMarkers"] as Map<String, Any?>
    val source = sourceFile("CausalHttp.kt").readText()
    val structured = markers["structured"] as String
    assertTrue("the structured marker the CLI parses", source.contains(structured.trim()))
    assertTrue("the frozen runner contract", source.contains(markers["runnerContract"] as String))
    assertTrue(
      "the structured marker must go to stderr; the CLI's verdict path reads stderr",
      source.contains("System.err.println(\n              \"$structured"),
    )
  }

  // --- helpers ---------------------------------------------------------------

  private fun assertVectorPairs(pairs: List<Map<String, Any?>>, label: String) {
    for (case in pairs) {
      val name = case["name"] ?: Json.encode(case["input"])
      assertEquals(
        "$label case $name",
        canonical(case["expect"]),
        canonical(redactCaptureValue(case["input"])),
      )
    }
  }

  private fun bodyOf(input: Map<String, Any?>): ByteArray? {
    val repeat = input["bodyRepeat"]
    if (repeat != null) {
      @Suppress("UNCHECKED_CAST") val spec = repeat as List<Any?>
      return (spec[0] as String).repeat(number(spec[1])).toByteArray()
    }
    return (input["body"] as String?)?.toByteArray()
  }

  /** `{"repeat": ["a", 8192]}` in an expectation stands for the expanded string. */
  private fun expandRepeats(value: Any?): Any? =
    when (value) {
      is List<*> -> value.map(::expandRepeats)
      is Map<*, *> -> {
        val repeat = value["repeat"]
        if (value.size == 1 && repeat is List<*>) {
          (repeat[0] as String).repeat(number(repeat[1]))
        } else {
          value.entries.associate { (key, child) -> key.toString() to expandRepeats(child) }
        }
      }
      else -> value
    }

  /**
   * Recursively key sorted JSON with nulls kept as data. `Json.encode` drops a
   * null map entry because the SDK's own optional event fields rely on that, so
   * a plain encode would compare a dropped key equal to a present null, which is
   * the exact distinction these vectors exist to pin.
   */
  private fun canonical(value: Any?): String = Json.encode(sortKeys(markJsonNulls(value)))

  private fun sortKeys(value: Any?): Any? =
    when (value) {
      is List<*> -> value.map(::sortKeys)
      is Map<*, *> -> {
        val out = LinkedHashMap<String, Any?>()
        for (key in value.keys.map { it.toString() }.sorted()) out[key] = sortKeys(value[key])
        out
      }
      else -> value
    }

  private fun number(value: Any?): Int =
    when (value) {
      is Number -> value.toInt()
      else -> throw AssertionError("expected a number, got $value")
    }

  private fun sourceFile(name: String): File =
    File(sdkRoot(), "reproit-android/src/main/kotlin/com/reproit/android/$name")

  private fun loadVectors(): Map<String, Any?> {
    val file = File(sdkRoot(), "capture-behavior-v1.json")
    @Suppress("UNCHECKED_CAST") return Json.decode(file.readText()) as Map<String, Any?>
  }

  /**
   * Walk up from the working directory to the `sdk/` tree. The host runner is
   * invoked from wherever the caller happens to stand, and a vector file that
   * silently failed to load would make every case above pass vacuously, so a
   * miss is an error rather than a skip.
   */
  private fun sdkRoot(): File {
    var here: File? = File(System.getProperty("user.dir")).absoluteFile
    while (here != null) {
      val candidate = File(here, "sdk/capture-behavior-v1.json")
      if (candidate.isFile) return candidate.parentFile
      if (File(here, "capture-behavior-v1.json").isFile) return here
      here = here.parentFile
    }
    throw AssertionError(
      "capture-behavior-v1.json not found above ${System.getProperty("user.dir")}"
    )
  }
}
