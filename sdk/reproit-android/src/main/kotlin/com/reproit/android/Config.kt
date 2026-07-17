package com.reproit.android

/**
 * Configuration for [ReproIt.init]. Field names and defaults mirror the web SDK
 * (`sdk/reproit-web.js`) and the Flutter SDK so behavior is consistent across platforms.
 */
data class ReproItConfig(
  /** Identifies the app in the cloud (the `appId` in every batch). Required. */
  val appId: String,

  /** `POST <endpoint>/v1/events`. If null, events go only to [onEvent]/log. */
  val endpoint: String? = null,

  /** Bearer token sent as `Authorization: Bearer <apiKey>` when set. */
  val apiKey: String? = null,

  /** User-visible application version stamped into `ctx.build.version`. */
  val buildVersion: String? = null,

  /** Source revision stamped into `ctx.build.commit`. */
  val buildCommit: String? = null,

  /**
   * Dev hook / custom transport; called for every event in addition to (or instead of, when
   * [endpoint] is null) the HTTP sink. The map is the event exactly as it will be serialized.
   */
  val onEvent: ((Map<String, Any?>) -> Unit)? = null,

  /** Fraction of sessions that report (0..1). Decided once at init. */
  val sampleRate: Double = 1.0,

  /** Max distinct labels captured per state (matches the runners). */
  val maxLabels: Int = 24,

  /** Labels longer than this are ignored (matches the runners). */
  val maxLabelLen: Int = 40,

  /** Max length of the action trail kept for repro paths. */
  val pathCap: Int = 60,

  /** How often batched events are flushed, in milliseconds. */
  val flushMs: Long = 5000,

  /** When true, only signatures are sent (no human-readable labels). */
  val redactLabels: Boolean = false,

  /** Settle window: snapshot once the UI has been quiet this long, in ms. */
  val debounceMs: Long = 350,
)
