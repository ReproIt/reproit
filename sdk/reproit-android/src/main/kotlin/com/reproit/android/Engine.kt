package com.reproit.android

/**
 * Platform-agnostic core of the SDK. Holds the state-graph state machine, label reduction, batching
 * and payload building. Has NO `android.*` imports so the behavior is unit-testable on the host
 * JVM; the Android layer ([ReproIt]) feeds it raw node data and supplies the wall clock +
 * transport.
 *
 * Mirrors `sdk/reproit-web.js` and `sdk/reproit_flutter/lib/reproit_flutter.dart`.
 */

/** One observed accessibility node, as the platform layer reads it. */
data class RawNode(
  /** contentDescription ?? text, already non-null (empty string if neither). */
  val name: String,
  /** true if the underlying view is clickable (`View.isClickable`). */
  val tappable: Boolean,
)

/** Reduced snapshot: a signature and capped unique display labels. */
data class Snapshot(val sig: String, val labels: List<String>)

/** One step in the graph trail kept for repros. */
data class Step(val sig: String, val action: String, val label: String? = null) {
  fun toMap(): Map<String, Any?> = mapOf("sig" to sig, "action" to action, "label" to label)
}

data class PendingStep(val action: String, val label: String? = null) {
  fun toStep(sig: String): Step = Step(sig, action, label)
}

class Engine(
  private val cfg: ReproItConfig,
  /** Wall clock in epoch milliseconds; injectable for tests. */
  private val now: () -> Long = { System.currentTimeMillis() },
  /**
   * Transport for a serialized batch body. Receives the JSON string; returns true on success. The
   * default is a no-op (the Android layer wires HTTP).
   */
  private val transport: (String) -> Boolean = { true },
  /** Optional plain-text logger used only when there is no endpoint/onEvent. */
  private val log: ((String) -> Unit)? = null,
) {
  private val queue = ArrayList<Map<String, Any?>>()
  private val path = ArrayList<Step>()
  private var currentSig: String? = null
  private var pending: PendingStep? = null
  private var batchSequence = 0L

  /**
   * App-declared invariants (see [ReproIt.invariant]): predicates that must hold in every visited
   * state. A plain SDK-owned store, idempotent by id and insertion-ordered; INERT in production
   * (never evaluated) and consulted only when the Android layer detects it is running under the
   * fuzzer, so registration is zero-overhead. Kept in the pure-Kotlin core so the registry +
   * evaluation are host-testable; the fuzzer gate + log emission live in the android layer
   * ([ReproIt]).
   */
  private val invariants = LinkedHashMap<String, () -> Boolean>()

  /**
   * PII-safe context dimensions sent with each batch (the "which users" answer). Insertion-ordered
   * and merged in place; included in each finding event's `context` field, exactly like the
   * Flutter/web SDKs.
   */
  private val context = LinkedHashMap<String, Any?>()

  init {
    val build = LinkedHashMap<String, String>()
    cfg.buildVersion?.takeIf { it.isNotBlank() }?.let { build["version"] = it }
    cfg.buildCommit?.takeIf { it.isNotBlank() }?.let { build["commit"] = it }
    if (build.isNotEmpty()) context["build"] = build
  }

  val queueSize: Int
    get() = synchronized(queue) { queue.size }

  fun currentSignature(): String? = currentSig

  /** Read-only snapshot of the current context dimensions (for tests / debug). */
  fun context(): Map<String, Any?> = synchronized(context) { LinkedHashMap(context) }

  /** Set a single PII-safe context dimension (e.g. role, plan, a count bucket). */
  fun setContext(key: String, value: Any?) {
    synchronized(context) { context[key] = value }
  }

  /** Merge several context dimensions at once. */
  fun setContexts(values: Map<String, Any?>) {
    synchronized(context) { context.putAll(values) }
  }

  /**
   * Attach a hashed user id (so the cloud can group "these N users hit it" without storing
   * identity) plus optional context dimensions. The raw [userId] is never stored or sent; only a
   * SHA-256 hex prefix is kept as `uid`. Mirrors the Flutter SDK's `identify`.
   */
  fun identify(userId: String, context: Map<String, Any?>? = null) {
    synchronized(this.context) {
      this.context["uid"] = hashUid(userId)
      if (context != null) this.context.putAll(context)
    }
  }

  private fun hashUid(userId: String): String {
    val digest =
      java.security.MessageDigest.getInstance("SHA-256").digest(userId.toByteArray(Charsets.UTF_8))
    val sb = StringBuilder(32)
    // First 8 bytes -> 16 lowercase hex chars (matches Flutter's substring(0, 16)).
    for (i in 0 until 8) sb.append("%02x".format(digest[i]))
    return sb.toString()
  }

  /**
   * Accessible name reduction shared with the snapshot path. Trim, take the first line, drop
   * empties and labels longer than [ReproItConfig.maxLabelLen].
   */
  fun cleanLabel(raw: String): String? {
    val first = raw.trim().substringBefore('\n').trim()
    if (first.isEmpty() || first.length > cfg.maxLabelLen) return null
    return first
  }

  /**
   * Reduce a flat list of visible nodes (pre-order: parents before children) plus the captured
   * structural [tree] into a [Snapshot].
   *
   * The signature is STRUCTURAL: the canonical descriptor of [tree] prefixed by the screen [anchor]
   * (route), byte-identical to the Rust oracle and the other SDKs (docs/signature.md). Localized
   * text never enters the hash.
   *
   * The flat [nodes] list is used only for the display-only `labels` field (`map --show`): it
   * dedupes labels and caps to [ReproItConfig.maxLabels]. Labels do NOT affect the signature.
   */
  fun reduce(nodes: List<RawNode>, tree: Signature.Node, anchor: String? = null): Snapshot {
    val seen = LinkedHashSet<String>()
    for (n in nodes) {
      val label = cleanLabel(n.name)
      if (label == null) continue
      seen.add(label)
    }
    val unique = seen.toList()
    val sig = Signature.of(anchor, tree)
    return Snapshot(sig, unique.take(cfg.maxLabels))
  }

  /**
   * Register an app invariant, idempotent by [id] (re-registering an id replaces it). [test]
   * returns true when the invariant HOLDS; returning false or throwing marks it VIOLATED (a thrown
   * exception's message becomes the finding message). Mirrors the web SDK's `ReproIt.invariant`.
   * Evaluation is gated by the fuzzer in the android layer, so this is inert in production.
   */
  fun registerInvariant(id: String, test: () -> Boolean) {
    synchronized(invariants) { invariants[id] = test }
  }

  /**
   * Evaluate every registered invariant; return one `{id,message}` entry per VIOLATED invariant
   * (held ones are omitted). Each predicate is isolated so one throwing predicate cannot suppress
   * the others. Does NOT apply the fuzzer gate (that lives in [ReproIt]); host-testable.
   */
  fun evaluateInvariants(): List<Map<String, Any?>> {
    val snapshot = synchronized(invariants) { LinkedHashMap(invariants) }
    val out = ArrayList<Map<String, Any?>>()
    for ((id, test) in snapshot) {
      var ok = true
      var message = ""
      try {
        ok = test()
      } catch (t: Throwable) {
        ok = false
        message = t.message ?: t.toString()
      }
      if (!ok) out.add(linkedMapOf("id" to id, "message" to message))
    }
    return out
  }

  /**
   * The `REPROIT_INVARIANT` marker line to log when one or more invariants are violated, else null
   * (silent). The emitted sig is left empty ("") so the mobile runner substitutes the sig it is
   * currently on. The android layer logs this to logcat only under the fuzzer; the Rust core is
   * never touched.
   */
  fun invariantMarker(): String? {
    val items = evaluateInvariants()
    if (items.isEmpty()) return null
    val obj = LinkedHashMap<String, Any?>()
    obj["sig"] = ""
    obj["items"] = items
    return "REPROIT_INVARIANT " + Json.encode(obj)
  }

  /** Record the action a tap implies; consumed by the next state change. */
  fun noteTap(selector: String?, label: String?) {
    pending =
      PendingStep(
        action = if (selector != null && selector.isNotEmpty()) "tap:$selector" else "tap:?",
        label = label,
      )
  }

  /** Record an explicit navigation action. */
  fun noteNav() {
    pending = PendingStep("nav")
  }

  /**
   * Observe a reduced snapshot. If the signature changed (or this is the first observation), record
   * an edge and advance the current state. `firstAction` is the action used for the very first
   * observed state ("load").
   */
  fun observe(snap: Snapshot, firstAction: String = "load") {
    val cur = currentSig
    if (cur == null) {
      currentSig = snap.sig
      emitEdge(from = null, action = firstAction, to = snap, append = true)
      return
    }
    if (snap.sig == cur) return
    val step = pending ?: PendingStep("auto")
    pending = null
    emitEdge(from = cur, step = step, to = snap, append = true)
    currentSig = snap.sig
  }

  private fun emitEdge(from: String?, action: String, to: Snapshot, append: Boolean) {
    emitEdge(from, PendingStep(action), to, append)
  }

  private fun emitEdge(from: String?, step: PendingStep, to: Snapshot, append: Boolean) {
    if (append) {
      path.add(step.toStep(from ?: ""))
      if (path.size > cfg.pathCap) path.removeAt(0)
    }
    val ev = LinkedHashMap<String, Any?>()
    ev["kind"] = "edge"
    if (from != null) ev["from"] = from
    ev["action"] = step.action
    if (!cfg.redactLabels && step.label != null) ev["label"] = step.label
    ev["to"] = to.sig
    if (!cfg.redactLabels) ev["labels"] = to.labels
    ev["t"] = now()
    enqueue(ev)
  }

  /**
   * Record an error event carrying the current signature and graph path. [stack] is capped to 8
   * lines. Returns the event (useful for tests / for the caller to flush synchronously before a
   * crash).
   */
  fun recordError(
    message: String,
    stack: List<String>,
    source: String = "",
    line: Int = 0,
    /**
     * PII-safe tier-3 on-error context (input fingerprints under `context.fingerprint`). Omitted
     * from the wire when null/empty.
     */
    context: Map<String, Any?>? = null,
  ): Map<String, Any?> {
    val ev = LinkedHashMap<String, Any?>()
    ev["kind"] = "error"
    // A genuine uncaught error IS the `crash` oracle firing; tag it so the
    // cloud can gate ingest on oracle-grade findings.
    ev["oracle"] = "crash"
    ev["sig"] = currentSig ?: ""
    // Include the in-flight action: a click whose handler throws synchronously
    // sets `pendingAction` but crashes before its debounced observe records it,
    // so the bare path stops one step short of the crashing tap.
    val pathOut = path.map { it.toMap() }.toMutableList()
    pending?.let { pathOut.add(it.toStep(currentSig ?: "").toMap()) }
    ev["path"] = pathOut
    ev["message"] = message
    ev["stack"] = stack.take(8)
    ev["source"] = source
    ev["line"] = line
    if (context != null && context.isNotEmpty()) ev["context"] = context
    ev["t"] = now()
    enqueue(ev)
    return ev
  }

  /** Capture the current structural state as a tester-observed bug. */
  fun captureBug(context: Map<String, Any?>? = null): Map<String, Any?>? {
    val sig = currentSig ?: return null
    val pathOut = path.map { it.toMap() }.toMutableList()
    pending?.let { pathOut.add(it.toStep(sig).toMap()) }
    val trigger = (pathOut.lastOrNull()?.get("action") as? String) ?: "load"
    val ev =
      linkedMapOf<String, Any?>(
        "kind" to "error",
        "oracle" to "tester-capture",
        "sig" to sig,
        "path" to pathOut,
        "message" to "Tester observed a bug in this state",
        "findingIdentity" to
          linkedMapOf(
            "oracle" to "tester-capture",
            "invariant" to "tester-observed-failure",
            "kind" to "structural-state",
            "message" to "",
            "frame" to "",
            "trigger" to trigger,
            "boundary" to sig,
          ),
        "t" to now(),
      )
    if (context != null && context.isNotEmpty()) ev["context"] = context
    enqueue(ev)
    return ev
  }

  fun captureContractBug(identity: String, message: String): Map<String, Any?>? {
    val sig = currentSig ?: return null
    val pathOut = path.map { it.toMap() }.toMutableList()
    pending?.let { pathOut.add(it.toStep(sig).toMap()) }
    val trigger = (pathOut.lastOrNull()?.get("action") as? String) ?: "load"
    val ev =
      linkedMapOf<String, Any?>(
        "kind" to "error",
        "oracle" to "invariant",
        "sig" to sig,
        "path" to pathOut,
        "message" to message,
        "findingIdentity" to
          linkedMapOf(
            "oracle" to "invariant",
            "invariant" to identity,
            "kind" to "structural-contract",
            "message" to message,
            "frame" to "",
            "trigger" to trigger,
            "boundary" to sig,
          ),
        "t" to now(),
      )
    enqueue(ev)
    return ev
  }

  private fun enqueue(ev: Map<String, Any?>) {
    try {
      cfg.onEvent?.invoke(ev)
    } catch (_: Throwable) {}
    if (cfg.endpoint == null) {
      if (cfg.onEvent == null) log?.invoke("reproit " + Json.encode(ev))
      return
    }
    synchronized(queue) {
      queue.add(ev)
      if (queue.size >= 50) {
        // flush inline to bound memory; the timer also flushes.
      }
    }
    if (queueSize >= 50) flush()
  }

  /** Build a strict version 1 event batch for the currently queued capture records. */
  fun buildBatch(events: List<Map<String, Any?>>): String {
    val sentAt = now()
    val sequence = synchronized(this) { ++batchSequence }
    val batchId = "sdk-$sentAt-$sequence"
    val ctx = synchronized(context) { if (context.isEmpty()) null else LinkedHashMap(context) }
    val frames =
      events.mapIndexed { index, event ->
        linkedMapOf<String, Any?>(
          "runId" to batchId,
          "sequence" to index + 1,
          "scope" to linkedMapOf("domain" to "shared"),
          "event" to protocolEvent(event, ctx ?: emptyMap()),
        )
      }
    return Json.encode(
      linkedMapOf<String, Any?>(
        "version" to 1,
        "batchId" to batchId,
        "appId" to cfg.appId,
        "frames" to frames,
        "evidence" to emptyList<Any?>(),
      )
    )
  }

  private fun protocolEvent(
    event: Map<String, Any?>,
    batchContext: Map<String, Any?>,
  ): Map<String, Any?> {
    if (event["kind"] == "edge") {
      return linkedMapOf(
        "kind" to "graph-edge",
        "from" to (event["from"] ?: "∅"),
        "action" to (event["action"] ?: "auto"),
        "to" to (event["to"] ?: "?"),
      )
    }
    if (event["kind"] != "error") {
      return linkedMapOf("kind" to "stream-defect", "reason" to "invalid-event")
    }

    val path = (event["path"] as? List<*>)?.filterIsInstance<Map<String, Any?>>() ?: emptyList()
    val message = event["message"]?.toString() ?: ""
    val identity =
      event["findingIdentity"]
        ?: linkedMapOf(
          "oracle" to (event["oracle"]?.toString() ?: "crash"),
          "invariant" to "no-exception",
          "kind" to "exception",
          "message" to structuralMessage(message),
          "frame" to "",
          "trigger" to (path.lastOrNull()?.get("action")?.toString() ?: ""),
          "boundary" to null,
        )
    val findingContext = LinkedHashMap(batchContext)
    @Suppress("UNCHECKED_CAST")
    (event["context"] as? Map<String, Any?>)?.let { findingContext.putAll(it) }
    return linkedMapOf(
      "kind" to "finding",
      "signature" to (event["sig"]?.toString() ?: "?"),
      "message" to message,
      "identity" to identity,
      "path" to
        path.map { step ->
          linkedMapOf(
            "signature" to (step["sig"] ?: "?"),
            "action" to (step["action"] ?: "auto"),
            "label" to step["label"],
          )
        },
      "context" to findingContext,
    )
  }

  private fun structuralMessage(message: String): String =
    message.replace(Regex("([\"']).*?\\1"), "<q>").replace(Regex("[0-9][0-9.,]*"), "#")

  /**
   * Drain the queue and ship it via [transport]. On failure the batch is re-queued ahead of newer
   * events for one retry (mirrors the Flutter SDK).
   */
  fun flush() {
    if (cfg.endpoint == null) {
      synchronized(queue) { queue.clear() }
      return
    }
    val batch: List<Map<String, Any?>>
    synchronized(queue) {
      if (queue.isEmpty()) return
      batch = ArrayList(queue)
      queue.clear()
    }
    val body = buildBatch(batch)
    val ok =
      try {
        transport(body)
      } catch (_: Throwable) {
        false
      }
    if (!ok) {
      synchronized(queue) { queue.addAll(0, batch) }
    }
  }
}
