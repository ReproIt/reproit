package com.reproit.android

import java.util.Locale
import java.util.TimeZone

/**
 * Universal causal capture batch (`capture-batch-v1`), the Android port of
 * `sdk/reproit-recorder-node/index.js` plus the batch assembly in
 * `sdk/reproit-backend-node/capture.js`.
 *
 * A failure occurrence that carries recorded dependency exchanges ships on this
 * contract (`POST <endpoint>/v1/capture-batches`) instead of the legacy event
 * batch, because only this shape survives the projection back into a replayable
 * capture. Dependency exchanges are conditional evidence.
 *
 * Pure Kotlin with no `android.*` import, so the emitted batch is host-testable
 * against the protocol validator.
 */

/** The ingest protocol token charset (`validate_token` in reproit-protocol). */
private val TOKEN = Regex("^[A-Za-z0-9._:-]{1,128}$")

internal fun validToken(value: String?): Boolean = value != null && TOKEN.matches(value)

/** Recorder bounds. The protocol allows more; these keep a mobile batch small. */
private const val MAX_BATCH_EVENTS = 1024

/**
 * Incremental recorder for one occurrence. Mirrors the Node recorder's event
 * identity (`evt_<emitter>_<sequence>`), monotonic stamping, and causal parent
 * chaining exactly, so a batch from Android is indistinguishable in shape from
 * one emitted by the backend SDKs.
 */
internal class CaptureRecorder(
  private val batchId: String,
  private val projectId: String,
  private val sessionId: String,
  private val emitterId: String,
  private val emitterComponent: String,
  private val emitterRuntime: String,
  private val observedAt: String,
  private val deployment: Map<String, Any?>?,
  private val capabilities: List<Map<String, Any?>>,
) {
  private val events = ArrayList<Map<String, Any?>>()
  private var sequence = 1
  private var finished = false

  /** Record one event and return its id, or null once the batch is bounded out. */
  fun record(
    event: Map<String, Any?>,
    parent: String? = null,
    traceId: String? = null,
    monotonicNs: Long? = null,
  ): String? {
    if (finished || events.size >= MAX_BATCH_EVENTS) return null
    val ordinal = sequence++
    val id = "evt_${emitterId}_$ordinal"
    val captured = LinkedHashMap<String, Any?>()
    captured["id"] = id
    captured["sequence"] = ordinal
    captured["monotonicNs"] = monotonicNs ?: ordinal.toLong()
    captured["causalParentIds"] = if (parent == null) emptyList<String>() else listOf(parent)
    if (traceId != null) captured["traceId"] = traceId
    captured["event"] = event
    events.add(captured)
    return id
  }

  fun operationStart(name: String, parent: String?, traceId: String?, monotonicNs: Long?): String? =
    record(linkedMapOf("kind" to "operation-start", "name" to name), parent, traceId, monotonicNs)

  fun trigger(
    trigger: String,
    subject: String,
    value: Map<String, Any?>?,
    parent: String?,
    traceId: String?,
    monotonicNs: Long?,
  ): String? {
    val event = linkedMapOf<String, Any?>("kind" to "trigger", "trigger" to trigger)
    event["subject"] = subject
    if (value != null) event["value"] = value
    return record(event, parent, traceId, monotonicNs)
  }

  fun checkpoint(
    name: String,
    attributes: Map<String, Any?>,
    parent: String?,
    traceId: String?,
    monotonicNs: Long?,
  ): String? =
    record(
      linkedMapOf("kind" to "checkpoint", "name" to name, "attributes" to attributes),
      parent,
      traceId,
      monotonicNs,
    )

  fun dependency(
    system: String,
    operation: String,
    subject: String,
    value: Map<String, Any?>?,
    parent: String?,
    traceId: String?,
    monotonicNs: Long?,
  ): String? {
    val event = linkedMapOf<String, Any?>("kind" to "dependency", "system" to system)
    event["operation"] = operation
    event["subject"] = subject
    if (value != null) event["value"] = value
    return record(event, parent, traceId, monotonicNs)
  }

  fun operationEnd(
    name: String,
    outcome: String,
    parent: String?,
    traceId: String?,
    monotonicNs: Long?,
  ): String? =
    record(
      linkedMapOf("kind" to "operation-end", "name" to name, "outcome" to outcome),
      parent,
      traceId,
      monotonicNs,
    )

  fun failure(
    failure: Map<String, Any?>,
    parent: String?,
    traceId: String?,
    monotonicNs: Long?,
  ): String? = record(linkedMapOf("kind" to "observation", "failure" to failure), parent, traceId, monotonicNs)

  fun finish(): Map<String, Any?> {
    finished = true
    val batch = LinkedHashMap<String, Any?>()
    batch["version"] = 1
    batch["batchId"] = batchId
    batch["projectId"] = projectId
    batch["sessionId"] = sessionId
    batch["emitter"] =
      linkedMapOf(
        "id" to emitterId,
        "kind" to "runtime-sdk",
        "component" to emitterComponent,
        "runtime" to emitterRuntime,
      )
    if (deployment != null) batch["deployment"] = deployment
    batch["observedAt"] = observedAt
    batch["policy"] =
      linkedMapOf("consent" to "application-telemetry", "retentionClass" to "standard")
    batch["capabilities"] = capabilities
    batch["events"] = events
    batch["artifacts"] = emptyList<Any?>()
    return batch
  }
}

/** A replayable captured value in the protocol's representation vocabulary. */
internal fun replayableValue(value: Any?): Map<String, Any?> =
  linkedMapOf(
    "representation" to "replayable",
    "value" to value,
    "redaction" to "redacted-at-source",
  )

/** A structural captured value: shape retained, contents deliberately absent. */
internal fun structuralValue(shape: Any?): Map<String, Any?> =
  linkedMapOf("representation" to "structural", "shape" to shape)

/** One recorded outbound exchange with its ordering and subject metadata. */
internal data class CapturedExchange(
  val subject: String,
  val exchange: Map<String, Any?>,
  val atMs: Long,
  val monotonicNs: Long,
)

/**
 * The determinism envelope: where and when the capture happened, plus a seed
 * that makes REPLAY runs deterministic. Honesty note carried from the backend
 * SDKs: the seed does not reproduce the randomness the app drew in production,
 * it pins the replay's.
 */
internal fun determinismEnvelope(
  observedAtMs: Long,
  osRelease: String,
  arch: String,
  replaySeed: String,
  imageDigest: String?,
): MutableMap<String, Any?> {
  val envelope = LinkedHashMap<String, Any?>()
  envelope["observedAtMs"] = observedAtMs
  envelope["tz"] = TimeZone.getDefault().id
  envelope["runtime"] = "android"
  envelope["os"] = osRelease
  envelope["arch"] = arch
  envelope["replaySeed"] = replaySeed
  if (validToken(imageDigest)) envelope["imageDigest"] = imageDigest
  return envelope
}

/**
 * Assemble one failure occurrence into a capture batch: the UI action that
 * triggered it, the determinism envelope, every recorded dependency exchange in
 * order, and the failure observation carrying the oracle signature.
 *
 * Returns null when the identifiers the ingest protocol requires are unusable,
 * so a misconfigured app stops locally instead of emitting a batch the server
 * will reject.
 */
internal fun buildFailureCaptureBatch(
  appId: String,
  sessionId: String,
  operation: String,
  triggerAction: String,
  triggerValue: Any? = null,
  signature: String,
  summary: String,
  observationPoint: String,
  exchanges: List<CapturedExchange>,
  envelope: Map<String, Any?>,
  buildVersion: String?,
  buildCommit: String?,
  observedAtIso: String,
  batchSequence: Long,
  observedAtMs: Long,
): Map<String, Any?>? {
  if (!validToken(appId)) return null
  val batchId = "cb-android-$observedAtMs-$batchSequence"
  if (!validToken(batchId)) return null
  val deployment = LinkedHashMap<String, Any?>()
  if (validToken(buildVersion)) deployment["version"] = buildVersion
  if (validToken(buildCommit)) deployment["commit"] = buildCommit
  val capabilities =
    mutableListOf<Map<String, Any?>>(
      linkedMapOf("capability" to "user-interface", "completeness" to "complete")
    )
  // Declared only when exchanges were actually recorded, so the capsule
  // completeness model never over-claims on a capture without them.
  if (exchanges.isNotEmpty()) {
    capabilities.add(
      linkedMapOf(
        "capability" to "network",
        "completeness" to "complete",
        "detail" to "outbound dependency exchanges recorded with responses",
      )
    )
  }
  val recorder =
    CaptureRecorder(
      batchId = batchId,
      projectId = appId,
      sessionId = sessionId,
      emitterId = "mobile-android",
      emitterComponent = "mobile",
      emitterRuntime = "android",
      observedAt = observedAtIso,
      deployment = if (deployment.isEmpty()) null else deployment,
      capabilities = capabilities,
    )
  var parent = recorder.operationStart(operation, null, sessionId, observedAtMs)
  parent =
    recorder.trigger(
      "ui-action",
      operation,
      replayableValue(triggerValue ?: linkedMapOf("action" to triggerAction)),
      parent,
      sessionId,
      observedAtMs,
    )
  parent = recorder.checkpoint("determinism-envelope", envelope, parent, sessionId, observedAtMs)
  for (captured in exchanges) {
    parent =
      recorder.dependency(
        "service",
        "call",
        captured.subject,
        replayableValue(captured.exchange),
        parent,
        sessionId,
        captured.monotonicNs,
      )
  }
  parent = recorder.operationEnd(operation, "failed", parent, sessionId, observedAtMs)
  recorder.failure(
    linkedMapOf(
      "observation" to "exception",
      "authority" to "runtime-diagnosis",
      "summary" to summary,
      "signature" to signature,
      "observationPoint" to observationPoint,
      "artifactIds" to emptyList<String>(),
    ),
    parent,
    sessionId,
    observedAtMs,
  )
  return recorder.finish()
}

/** A bounded hex seed for the determinism envelope. */
internal fun replaySeedHex(random: () -> Long): String =
  java.lang.Long.toHexString(random()).padStart(16, '0').lowercase(Locale.ROOT).takeLast(16)
