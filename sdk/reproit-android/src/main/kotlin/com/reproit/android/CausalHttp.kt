package com.reproit.android

import java.io.File
import java.net.HttpURLConnection
import java.net.URI
import java.net.URL
import java.util.Locale

/** A dependency-free causal HTTP client for native Android applications. */
@Suppress("UNCHECKED_CAST")
class CausalHttp {
  data class Response(val status: Int, val headers: Map<String, String>, val body: ByteArray)

  private val lock = Any()
  private val actor = System.getenv("REPROIT_DEVICE") ?: "a"
  private val networkFile = System.getenv("REPROIT_NETWORK_FILE")
  private val capsulePath =
    (System.getenv("REPROIT_CAPSULE") ?: systemProperty("debug.reproit.capsule"))?.takeUnless {
      it == "__reproit_none__"
    }
  private val active =
    networkFile != null ||
      capsulePath != null ||
      System.getenv("REPROIT_CAUSAL") == "1" ||
      systemProperty("debug.reproit.fuzz") == "1"
  private val exchanges: List<Map<String, Any?>>
  private val used = mutableSetOf<Int>()
  private var previousAction = -1
  private var ordinal = 0

  /**
   * Production exchange recording, opt in through
   * [ReproItConfig.captureExchanges] and OFF by default. Independent of the
   * runner signals above: when a runner is driving the app this stays false and
   * the runner path below is byte-unchanged.
   */
  @Volatile private var capturing = false
  private val captured = ArrayList<CapturedExchange>()
  private val captureOrigin = System.nanoTime()

  /** Enable or disable production recording. Called once from [ReproIt.init]. */
  internal fun setCapturing(enabled: Boolean) {
    capturing = enabled && !active
  }

  /** Whether production recording is live (false under a runner). */
  internal fun capturing(): Boolean = capturing

  /** Drain the recorded exchanges for one failure occurrence. */
  internal fun drainCaptured(): List<CapturedExchange> =
    synchronized(captured) {
      val out = ArrayList(captured)
      captured.clear()
      out
    }

  private fun retain(exchange: CapturedExchange) {
    synchronized(captured) {
      captured.add(exchange)
      while (captured.size > MAX_CAPTURED_EXCHANGES) captured.removeAt(0)
    }
  }

  init {
    exchanges =
      try {
        val root = Json.decode(File(capsulePath!!).readText()) as Map<String, Any?>
        root["exchanges"] as? List<Map<String, Any?>> ?: emptyList()
      } catch (_: Throwable) {
        emptyList()
      }
    if (active) {
      android.util.Log.d(
        "reproit",
        "REPROIT:CAPABILITIES {\"http\":{\"status\":\"captured\"},"
          + "\"http_replay\":{\"status\":\"captured\"}}",
      )
    }
  }

  @JvmOverloads
  fun request(
    url: String,
    method: String = "GET",
    headers: Map<String, String> = emptyMap(),
    body: ByteArray? = null,
    connectTimeoutMs: Int = 8000,
    readTimeoutMs: Int = 8000,
  ): Response {
    if (!active) {
      val response = live(url, method, headers, body, connectTimeoutMs, readTimeoutMs)
      // Production capture: record what replay will have to serve. Never lets a
      // capture defect reach the caller; the response is already in hand.
      if (capturing) {
        try {
          retain(
            CapturedExchange(
              subject = URI(url).host ?: url,
              exchange =
                captureHttpExchange(
                  method = method,
                  url = url,
                  requestHeaders = headers,
                  requestBody = body,
                  status = response.status,
                  responseHeaders = response.headers,
                  responseBody = response.body,
                ),
              atMs = System.currentTimeMillis(),
              monotonicNs = System.nanoTime() - captureOrigin,
            )
          )
        } catch (_: Throwable) {}
      }
      return response
    }
    val action = actionIndex()
    val currentOrdinal =
      synchronized(lock) {
        if (previousAction != action) {
          previousAction = action
          ordinal = 0
        }
        ordinal++
        ordinal - 1
      }
    if (capsulePath != null) {
      val match =
        synchronized(lock) {
          exchanges
            .withIndex()
            .firstOrNull { (index, exchange) ->
              index !in used &&
                exchange["required"] == true &&
                exchange["actor"] == actor &&
                number(exchange, "actionIndex", "action_index") == action &&
                exchange["method"].toString().equals(method, true) &&
                canonical(exchange["url"].toString()) == canonical(url)
            }
            ?.also { used += it.index }
            ?.value
        }
          ?: throw IllegalStateException(
            "CAPSULE:MISS ${method.uppercase(Locale.ROOT)} $url action=$action"
          )
      val responseBody = match["responseBody"] ?: match["response_body"]
      val bytes =
        if (responseBody is String) responseBody.toByteArray()
        else Json.encode(responseBody).toByteArray()
      @Suppress("UNCHECKED_CAST")
      val responseHeaders =
        (match["responseHeaders"] ?: match["response_headers"] ?: emptyMap<String, String>())
          as Map<String, String>
      android.util.Log.d("reproit", "CAPSULE:HIT ${match["id"] ?: ""}")
      return Response(number(match, "status"), responseHeaders, bytes)
    }
    val response = live(url, method, headers, body, connectTimeoutMs, readTimeoutMs)
    val exchange =
      linkedMapOf<String, Any?>(
        "id" to "$actor-$action-$currentOrdinal",
        "actor" to actor,
        "actionIndex" to action,
        "ordinal" to currentOrdinal,
        "protocol" to URI(url).scheme,
        "method" to method.uppercase(Locale.ROOT),
        "url" to url,
        "requestHeaders" to redactHeaders(headers),
        "requestBody" to bodyValue(body, headers),
        "status" to response.status,
        "responseHeaders" to redactHeaders(response.headers),
        "responseBody" to bodyValue(response.body, response.headers),
        "required" to true,
      )
    val marker = "REPROIT:EXCHANGE ${Json.encode(exchange)}"
    android.util.Log.d("reproit", marker)
    try {
      networkFile?.let { File(it).appendText(Json.encode(exchange) + "\n") }
    } catch (_: Throwable) {}
    return response
  }

  private fun live(
    url: String,
    method: String,
    headers: Map<String, String>,
    body: ByteArray?,
    connectMs: Int,
    readMs: Int,
  ): Response {
    val connection = URL(url).openConnection() as HttpURLConnection
    try {
      connection.requestMethod = method.uppercase(Locale.ROOT)
      connection.connectTimeout = connectMs
      connection.readTimeout = readMs
      for ((name, value) in headers) {
        connection.setRequestProperty(name, value)
      }
      if (body != null) {
        connection.doOutput = true
        connection.outputStream.use { it.write(body) }
      }
      val status = connection.responseCode
      val stream = if (status >= 400) connection.errorStream else connection.inputStream
      val bytes = stream?.use { it.readBytes() } ?: byteArrayOf()
      val responseHeaders =
        connection.headerFields
          .filterKeys { it != null }
          .mapValues { (_, values) -> values.joinToString(",") }
      return Response(status, responseHeaders, bytes)
    } finally {
      connection.disconnect()
    }
  }

  private fun actionIndex(): Int {
    systemProperty("debug.reproit.action")?.toIntOrNull()?.let {
      return it
    }
    return try {
      File(System.getenv("REPROIT_ACTION_FILE") ?: return 0).readText().trim().toInt()
    } catch (_: Throwable) {
      0
    }
  }

  private fun systemProperty(name: String): String? =
    try {
      val type = Class.forName("android.os.SystemProperties")
      (type.getMethod("get", String::class.java).invoke(null, name) as? String)?.takeIf {
        it.isNotBlank()
      }
    } catch (_: Throwable) {
      null
    }

  private fun canonical(raw: String): String =
    try {
      val uri = URI(raw)
      val query = uri.rawQuery?.split("&")?.sorted()?.joinToString("&")
      URI(
          uri.scheme?.lowercase(),
          uri.userInfo,
          uri.host?.lowercase(),
          uri.port,
          uri.path,
          query,
          uri.fragment,
        )
        .toString()
    } catch (_: Throwable) {
      raw
    }

  private fun number(value: Map<String, Any?>, vararg keys: String): Int {
    val found = keys.firstNotNullOfOrNull { value[it] }
    return (found as? Number)?.toInt() ?: found?.toString()?.toIntOrNull() ?: 0
  }

  private fun redactHeaders(headers: Map<String, String>) =
    headers.mapValues { (key, value) -> if (causalSecretField(key)) "<reproit:secret>" else value }

  private fun redact(value: Any?): Any? = redactCausalValue(value)

  private fun bodyValue(body: ByteArray?, headers: Map<String, String>): Any? {
    if (body == null || body.isEmpty()) return null
    val json =
      headers.entries.any { it.key.equals("content-type", true) && it.value.contains("json", true) }
    if (!json) return "<reproit:body:length=${body.size}>"
    return try {
      redact(Json.decode(body.toString(Charsets.UTF_8)))
    } catch (_: Throwable) {
      "<reproit:invalid-json>"
    }
  }
}

// `causalSecretField` and `redactCausalValue` now live in Exchange.kt so the
// runner's redaction rules compile and test on the host JVM. Behavior is
// unchanged; only the file changed.
