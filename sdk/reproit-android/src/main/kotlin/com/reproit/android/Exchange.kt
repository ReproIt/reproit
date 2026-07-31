package com.reproit.android

import java.security.MessageDigest
import java.util.Locale

/**
 * Outbound dependency exchange capture, the Android port of
 * `sdk/reproit-backend-node/instrument.js`.
 *
 * An exchange is the request the app sent and the response the dependency
 * returned. It is what hermetic local replay serves, so responses are captured
 * verbatim up to a fixed inline budget; an over-budget body keeps only its byte
 * count and sha256 and is marked truncated, and replay fails closed on it with a
 * named reason instead of guessing.
 *
 * Pure Kotlin with no `android.*` import so every bound and every redaction rule
 * is host-testable; [CausalHttp] supplies the transport.
 *
 * Two redaction vocabularies live in this file on purpose:
 *   - [redactCausalValue] keeps the `<reproit:...>` placeholders the RUNNER path
 *     has always emitted, so runner output stays byte-identical.
 *   - [redactCaptureValue] emits Node's `$reproit` metadata stub, because the
 *     replay matcher treats a `$reproit` marker as "any value stood here" and
 *     would compare a `<reproit:...>` string literally and diverge.
 */

/** Inline body budget per exchange side, byte-identical to the Node SDK. */
internal const val MAX_EXCHANGE_BODY_BYTES = 8 * 1024

/** Recorded headers per exchange side, byte-identical to the Node SDK. */
internal const val MAX_EXCHANGE_HEADERS = 32

/** Captured exchanges retained for one failure occurrence; drops oldest. */
internal const val MAX_CAPTURED_EXCHANGES = 32

/**
 * Node's secret-key vocabulary (`SECRET_PARTS` in reproit-backend-node/index.js).
 * Deliberately separate from [causalSecretField]: this list adds
 * `idempotencykey`, and changing the runner list would change runner output.
 */
private val CAPTURE_SECRET_PARTS =
  listOf(
    "password",
    "passwd",
    "secret",
    "token",
    "authorization",
    "cookie",
    "email",
    "phone",
    "apikey",
    "publishablekey",
    "privatekey",
    "accesskey",
    "signingkey",
    "idempotencykey",
  )

/** Fold a field name the way Node does: drop non-alphanumerics, lowercase. */
internal fun captureSecretField(key: String): Boolean {
  val compact = key.filter { it.isLetterOrDigit() }.lowercase(Locale.ROOT)
  return CAPTURE_SECRET_PARTS.any(compact::contains)
}

/**
 * Recursive structural redaction with Node's `$reproit` placeholder shape.
 * Secret-named fields keep their type and length so replay can still match
 * positionally; every other value passes through.
 */
internal fun redactCaptureValue(value: Any?): Any? =
  when (value) {
    is List<*> -> value.map(::redactCaptureValue)
    is Map<*, *> ->
      value.entries.associate { (key, child) ->
        key.toString() to
          if (captureSecretField(key.toString())) captureMetadata(child)
          else redactCaptureValue(child)
      }
    else -> value
  }

/** Node's `metadata()`: the typed stub that replaces a secret-named value. */
private fun captureMetadata(value: Any?): Map<String, Any?> {
  val type =
    when (value) {
      null -> "null"
      is Boolean -> "boolean"
      is Int,
      is Long -> "integer"
      is Double -> if (value == value.toLong().toDouble()) "integer" else "number"
      is String -> "string"
      is List<*> -> "array"
      is Map<*, *> -> "object"
      else -> "null"
    }
  val length: Any? =
    when (value) {
      is String -> value.codePointCount(0, value.length)
      is List<*> -> value.size
      else -> null
    }
  return mapOf("\$reproit" to linkedMapOf("redacted" to true, "type" to type, "length" to length))
}

internal fun sha256Hex(bytes: ByteArray): String {
  val digest = MessageDigest.getInstance("SHA-256").digest(bytes)
  val out = StringBuilder(64)
  for (byte in digest) out.append("%02x".format(byte))
  return out.toString()
}

/**
 * Bound one exchange body. Returns an empty map for an absent or empty body, the
 * provable identity only when the body exceeds [MAX_EXCHANGE_BODY_BYTES], and
 * otherwise the parsed JSON (when the content type declares it) or the text.
 */
internal fun boundedBody(body: ByteArray?, contentType: String?): Map<String, Any?> {
  if (body == null || body.isEmpty()) return emptyMap()
  if (body.size > MAX_EXCHANGE_BODY_BYTES) {
    return linkedMapOf(
      "bodyBytes" to body.size,
      "bodySha256" to sha256Hex(body),
      "truncated" to true,
    )
  }
  val text = body.toString(Charsets.UTF_8)
  if (contentType != null && contentType.contains("application/json")) {
    try {
      // Mark decoded nulls as DATA so the encoder keeps them: a response of
      // {"prices": null} must replay as {"prices": null}, not as a body with
      // the key missing, which would reproduce a different error.
      return linkedMapOf("body" to markJsonNulls(Json.decode(text)))
    } catch (_: Throwable) {
      // Declared JSON that does not parse is recorded as text below.
    }
  }
  return linkedMapOf("body" to text)
}

/**
 * Recursively replace decoded JSON nulls with [JsonNull] so [Json] encodes
 * them instead of dropping the key. Applies only to captured payloads, never
 * to the SDK's own optional event fields.
 */
internal fun markJsonNulls(value: Any?): Any? =
  when (value) {
    null -> JsonNull
    is Map<*, *> -> {
      val marked = LinkedHashMap<String, Any?>(value.size)
      for ((k, v) in value) marked[k.toString()] = markJsonNulls(v)
      marked
    }
    is List<*> -> value.map { markJsonNulls(it) }
    else -> value
  }

/** Bound and lowercase one exchange side's headers. */
internal fun boundedHeaders(headers: Map<String, String>): Map<String, Any?> {
  if (headers.isEmpty()) return emptyMap()
  val bounded = LinkedHashMap<String, Any?>()
  // The cap is defined over NAME SORTED order, never arrival order. Go recorded
  // a different subset on every run because it capped a randomized map before
  // sorting it, and a capsule whose recorded headers vary between runs cannot be
  // matched twice. A LinkedHashMap does not randomize, so the wrong subset here
  // would be stable and therefore even harder to notice.
  val sorted =
    headers.entries
      .map { it.key.lowercase(Locale.ROOT) to it.value }
      .sortedBy { it.first }
      .take(MAX_EXCHANGE_HEADERS)
  for ((name, value) in sorted) bounded[name] = value
  return linkedMapOf("headers" to bounded)
}

/** Read one header case-insensitively, the way Node's `headerValue` does. */
internal fun headerValue(headers: Map<String, String>, name: String): String? =
  headers.entries.firstOrNull { it.key.equals(name, ignoreCase = true) }?.value

/**
 * Build one redacted `{protocol, request, response}` exchange in the exact shape
 * the Node and Rust SDKs emit, so one replay engine serves all three.
 */
internal fun captureHttpExchange(
  method: String,
  url: String,
  requestHeaders: Map<String, String>,
  requestBody: ByteArray?,
  status: Int,
  responseHeaders: Map<String, String>,
  responseBody: ByteArray?,
): Map<String, Any?> {
  val request = LinkedHashMap<String, Any?>()
  request["method"] = method.uppercase(Locale.ROOT)
  request["url"] = url
  request.putAll(boundedHeaders(requestHeaders))
  request.putAll(boundedBody(requestBody, headerValue(requestHeaders, "content-type")))
  val response = LinkedHashMap<String, Any?>()
  response["status"] = status
  response.putAll(boundedHeaders(responseHeaders))
  response.putAll(boundedBody(responseBody, headerValue(responseHeaders, "content-type")))
  val exchange =
    linkedMapOf<String, Any?>("protocol" to "http", "request" to request, "response" to response)
  @Suppress("UNCHECKED_CAST")
  return redactCaptureValue(exchange) as Map<String, Any?>
}

/**
 * The runner-path secret vocabulary. Unchanged from its original home in
 * [CausalHttp] so the runner wire stays byte-identical; moved here only so the
 * pure logic compiles and tests on the host JVM without the Android SDK.
 */
internal fun causalSecretField(key: String): Boolean {
  val compact =
    key.lowercase(Locale.ROOT).filterNot { it == '-' || it == '_' || it == '.' || it == ' ' }
  return listOf(
      "password",
      "passwd",
      "secret",
      "token",
      "authorization",
      "cookie",
      "email",
      "phone",
      "apikey",
      "publishablekey",
      "privatekey",
      "accesskey",
      "signingkey",
    )
    .any(compact::contains)
}

internal fun redactCausalValue(value: Any?): Any? =
  when (value) {
    is List<*> -> value.map(::redactCausalValue)
    is Map<*, *> ->
      value.entries.associate { (key, child) ->
        key.toString() to
          if (causalSecretField(key.toString())) causalTypedValue(child)
          else redactCausalValue(child)
      }
    else -> value
  }

private fun causalTypedValue(value: Any?): String =
  when (value) {
    null -> "<reproit:null>"
    is String -> "<reproit:string:length=${value.codePointCount(0, value.length)}>"
    is Boolean -> "<reproit:boolean>"
    is Number -> "<reproit:number>"
    is List<*> -> "<reproit:array:length=${value.size}>"
    is Map<*, *> -> "<reproit:object:keys=${value.size}>"
    else -> "<reproit:unknown>"
  }
