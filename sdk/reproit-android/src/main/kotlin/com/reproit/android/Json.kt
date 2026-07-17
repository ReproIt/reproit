package com.reproit.android

/**
 * Minimal JSON encoder for the event payloads. Pure Kotlin (no `org.json`, which is an Android stub
 * on the host JVM and not available to plain `kotlinc`), so the JSON-shape tests run on the host.
 * Only encodes the value types the event model uses: String, Int, Long, Double, Boolean, null,
 * List, and Map.
 */
object Json {

  fun encode(value: Any?): String {
    val sb = StringBuilder()
    write(sb, value)
    return sb.toString()
  }

  private fun write(sb: StringBuilder, value: Any?) {
    when (value) {
      null -> sb.append("null")
      is String -> writeString(sb, value)
      is Boolean -> sb.append(value.toString())
      is Int,
      is Long -> sb.append(value.toString())
      is Double -> {
        // Emit integral doubles without a trailing ".0" so timestamps
        // and counts read as plain integers, matching the other SDKs.
        if (value == value.toLong().toDouble()) sb.append(value.toLong().toString())
        else sb.append(value.toString())
      }
      is Map<*, *> -> {
        sb.append('{')
        var first = true
        for ((k, v) in value) {
          if (v == null) continue // omit null fields (matches `from?`/`labels?`)
          if (!first) sb.append(',')
          first = false
          writeString(sb, k.toString())
          sb.append(':')
          write(sb, v)
        }
        sb.append('}')
      }
      is List<*> -> {
        sb.append('[')
        for ((i, v) in value.withIndex()) {
          if (i > 0) sb.append(',')
          write(sb, v)
        }
        sb.append(']')
      }
      else -> writeString(sb, value.toString())
    }
  }

  private fun writeString(sb: StringBuilder, s: String) {
    sb.append('"')
    for (c in s) {
      when (c) {
        '"' -> sb.append("\\\"")
        '\\' -> sb.append("\\\\")
        '\n' -> sb.append("\\n")
        '\r' -> sb.append("\\r")
        '\t' -> sb.append("\\t")
        '\u0008' -> sb.append("\\b")
        '\u000c' -> sb.append("\\f")
        else -> if (c < ' ') sb.append("\\u%04x".format(c.code)) else sb.append(c)
      }
    }
    sb.append('"')
  }

  /**
   * Minimal recursive-descent JSON decoder. Pure Kotlin (no `org.json`, which is an Android stub on
   * the host JVM and unavailable to plain `kotlinc`), so the parity test can parse
   * `signature_vectors.json` on the host. Returns the usual object graph: Map<String, Any?>,
   * List<Any?>, String, Double, Boolean, or null. Sufficient for the golden-vector schema; not a
   * full validator.
   */
  fun decode(text: String): Any? {
    val p = Parser(text)
    p.skipWs()
    val v = p.parseValue()
    p.skipWs()
    require(p.atEnd()) { "trailing JSON at index ${p.pos}" }
    return v
  }

  private class Parser(val s: String) {
    var pos = 0

    fun atEnd(): Boolean = pos >= s.length

    fun skipWs() {
      while (pos < s.length) {
        val c = s[pos]
        if (c == ' ' || c == '\t' || c == '\n' || c == '\r') pos++ else break
      }
    }

    fun parseValue(): Any? {
      skipWs()
      require(pos < s.length) { "unexpected end of JSON" }
      return when (s[pos]) {
        '{' -> parseObject()
        '[' -> parseArray()
        '"' -> parseString()
        't',
        'f' -> parseBool()
        'n' -> parseNull()
        else -> parseNumber()
      }
    }

    private fun parseObject(): Map<String, Any?> {
      expect('{')
      val out = LinkedHashMap<String, Any?>()
      skipWs()
      if (peek() == '}') {
        pos++
        return out
      }
      while (true) {
        skipWs()
        val key = parseString()
        skipWs()
        expect(':')
        val value = parseValue()
        out[key] = value
        skipWs()
        when (val c = next()) {
          ',' -> continue
          '}' -> break
          else -> error("expected ',' or '}' but got '$c' at ${pos - 1}")
        }
      }
      return out
    }

    private fun parseArray(): List<Any?> {
      expect('[')
      val out = ArrayList<Any?>()
      skipWs()
      if (peek() == ']') {
        pos++
        return out
      }
      while (true) {
        out.add(parseValue())
        skipWs()
        when (val c = next()) {
          ',' -> continue
          ']' -> break
          else -> error("expected ',' or ']' but got '$c' at ${pos - 1}")
        }
      }
      return out
    }

    private fun parseString(): String {
      expect('"')
      val sb = StringBuilder()
      while (true) {
        val c = next()
        when (c) {
          '"' -> break
          '\\' ->
            when (val e = next()) {
              '"' -> sb.append('"')
              '\\' -> sb.append('\\')
              '/' -> sb.append('/')
              'n' -> sb.append('\n')
              'r' -> sb.append('\r')
              't' -> sb.append('\t')
              'b' -> sb.append('\b')
              'f' -> sb.append('\u000c')
              'u' -> {
                val hex = s.substring(pos, pos + 4)
                pos += 4
                sb.append(hex.toInt(16).toChar())
              }
              else -> error("bad escape '\\$e' at ${pos - 1}")
            }
          else -> sb.append(c)
        }
      }
      return sb.toString()
    }

    private fun parseBool(): Boolean {
      return if (s.startsWith("true", pos)) {
        pos += 4
        true
      } else if (s.startsWith("false", pos)) {
        pos += 5
        false
      } else {
        error("invalid literal at $pos")
      }
    }

    private fun parseNull(): Any? {
      require(s.startsWith("null", pos)) { "invalid literal at $pos" }
      pos += 4
      return null
    }

    private fun parseNumber(): Double {
      val start = pos
      while (pos < s.length && s[pos] in "-+.eE0123456789") pos++
      return s.substring(start, pos).toDouble()
    }

    private fun peek(): Char = s[pos]

    private fun next(): Char = s[pos++]

    private fun expect(c: Char) {
      require(pos < s.length && s[pos] == c) { "expected '$c' at $pos" }
      pos++
    }
  }
}
