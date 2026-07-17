package com.reproit.android

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Test

class CausalRedactionTest {
  @Test
  fun explicitSecretKeysRedactWithoutHidingOrdinaryKeys() {
    val raw =
      linkedMapOf<String, Any?>(
        "apiKey" to "raw-api",
        "publishable-key" to "raw-pub",
        "private_key" to "raw-private",
        "access.key" to "raw-access",
        "signing key" to "raw-signing",
        "keyboardLayout" to "dvorak",
        "key" to "ordinary",
      )
    @Suppress("UNCHECKED_CAST") val safe = redactCausalValue(raw) as Map<String, Any?>
    for (name in listOf("apiKey", "publishable-key", "private_key", "access.key", "signing key")) {
      assertFalse(safe[name].toString().contains("raw-"))
    }
    assertEquals("dvorak", safe["keyboardLayout"])
    assertEquals("ordinary", safe["key"])
    assertFalse(Json.encode(safe).contains(Regex("raw-(api|pub|private|access|signing)")))
  }
}
