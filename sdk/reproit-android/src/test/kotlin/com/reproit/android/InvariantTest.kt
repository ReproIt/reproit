package com.reproit.android

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * App-invariant oracle (self-triggered model), host-testable half.
 *
 * The native fuzzer drives the Android app and cannot call the app's predicates, so the SDK
 * evaluates its OWN registered invariants on each settled state and, ONLY under the fuzzer, logs a
 * `REPROIT_INVARIANT` marker (on logcat) that runners/rn/runner.mjs scrapes into an
 * EXPLORE:INVARIANT line. The registry + evaluation + marker shape live in the pure-Kotlin [Engine]
 * (this test), while the fuzzer gate + `android.util.Log` emission live in the android-only
 * [ReproIt] layer (not host-testable, verified by the appium smoke). Both directions are asserted:
 * a VIOLATING invariant produces a marker, a CLEAN one is silent.
 */
class InvariantTest {
  private fun engine(): Engine = Engine(cfg = ReproItConfig(appId = "t"), now = { 0L })

  @Test
  fun evaluatesOnlyViolationsAndBuildsMarker() {
    val e = engine()
    e.registerInvariant("total-nonneg") { true } // holds
    e.registerInvariant("tab-selected") { false } // violated, empty message
    e.registerInvariant("cart") { throw IllegalStateException("cart went negative") } // via throw

    val items = e.evaluateInvariants()
    val byId = items.associate { it["id"] as String to it["message"] as String }
    assertEquals(setOf("tab-selected", "cart"), byId.keys)
    assertEquals("", byId["tab-selected"])
    assertEquals("cart went negative", byId["cart"])
    assertNull(byId["total-nonneg"]) // the held one never appears

    val marker = e.invariantMarker()!!
    assertTrue(marker.startsWith("REPROIT_INVARIANT "))
    val json = marker.removePrefix("REPROIT_INVARIANT ")
    // sig is emitted empty so the runner substitutes the sig it is on.
    assertTrue(json.contains("\"sig\":\"\""))
    assertTrue(json.contains("\"tab-selected\""))
    assertTrue(json.contains("cart went negative"))
  }

  @Test
  fun silentWhenAllHold() {
    val e = engine()
    e.registerInvariant("a") { true }
    e.registerInvariant("b") { true }
    assertTrue(e.evaluateInvariants().isEmpty())
    assertNull(e.invariantMarker())
  }

  @Test
  fun registrationIsIdempotentById() {
    val e = engine()
    e.registerInvariant("x") { true }
    e.registerInvariant("x") { false } // replaces the holding predicate
    val items = e.evaluateInvariants()
    assertEquals(1, items.size)
    assertEquals("x", items[0]["id"])
  }
}
