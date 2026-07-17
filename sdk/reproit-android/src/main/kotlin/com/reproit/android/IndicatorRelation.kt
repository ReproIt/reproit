package com.reproit.android

import kotlin.math.hypot
import kotlin.math.max
import kotlin.math.roundToInt

data class ReproItRect(val left: Double, val top: Double, val right: Double, val bottom: Double) {
  val width
    get() = right - left

  val height
    get() = bottom - top
}

data class ReproItIndicatorGeometry(
  val indicator: ReproItRect,
  val owner: ReproItRect,
  val container: ReproItRect,
  val animating: Boolean = false,
  val transformsResolved: Boolean = true,
)

internal object IndicatorRelations {
  private data class Contract(
    val dependentKey: String,
    val ownerKey: String,
    val containerKey: String,
    val maxGap: Double,
    val sample: () -> ReproItIndicatorGeometry?,
  )

  private val contracts = LinkedHashMap<String, Contract>()
  private val prior = HashMap<String, String>()
  private val counts = HashMap<String, Int>()

  @Synchronized
  fun register(
    id: String,
    dependentKey: String,
    ownerKey: String,
    containerKey: String,
    maxGap: Double,
    sample: () -> ReproItIndicatorGeometry?,
  ) {
    if (
      id.isEmpty() ||
        dependentKey.isEmpty() ||
        ownerKey.isEmpty() ||
        containerKey.isEmpty() ||
        !maxGap.isFinite() ||
        maxGap < 0
    )
      return
    contracts[id] = Contract(dependentKey, ownerKey, containerKey, maxGap, sample)
  }

  @Synchronized
  fun clear() {
    contracts.clear()
    prior.clear()
    counts.clear()
  }

  @Synchronized fun hasContracts(): Boolean = contracts.isNotEmpty()

  @Synchronized
  fun marker(): String? {
    val checks = ArrayList<Map<String, Any?>>()
    for ((id, c) in contracts.toSortedMap()) {
      val result = evaluate(c)
      val count = if (prior[id] == result.third) (counts[id] ?: 0) + 1 else 1
      prior[id] = result.third
      counts[id] = count
      if (count < 2) continue
      checks.add(
        linkedMapOf(
          "kind" to "indicator-anchor",
          "dependentKey" to c.dependentKey,
          "ownerKey" to c.ownerKey,
          "containerKey" to c.containerKey,
          "outcome" to result.first,
          "violation" to result.second,
        )
      )
    }
    if (checks.isEmpty()) return null
    return "REPROIT_RELATION " + Json.encode(linkedMapOf("stableSamples" to 2, "checks" to checks))
  }

  private fun evaluate(c: Contract): Triple<String, String?, String> {
    val g =
      try {
        c.sample()
      } catch (_: Throwable) {
        null
      }
    if (
      g == null ||
        g.animating ||
        !g.transformsResolved ||
        !valid(g.indicator) ||
        !valid(g.owner) ||
        !valid(g.container)
    ) {
      return Triple("UNKNOWN", null, "UNKNOWN")
    }
    val i = g.indicator
    val o = g.owner
    val box = g.container
    val escaped =
      i.left < box.left - .5 ||
        i.top < box.top - .5 ||
        i.right > box.right + .5 ||
        i.bottom > box.bottom + .5
    val dx = max(0.0, max(o.left - i.right, i.left - o.right))
    val dy = max(0.0, max(o.top - i.bottom, i.top - o.bottom))
    val detached = hypot(dx, dy) > c.maxGap + .5
    val violation = if (escaped) "escaped-container" else if (detached) "detached" else null
    val fp =
      listOf(i, o, box)
        .flatMap { listOf(it.left, it.top, it.width, it.height) }
        .joinToString(",") { (it * 2).roundToInt().toString() } + "|" + (violation ?: "valid")
    return Triple(if (violation == null) "VALID" else "PROVEN", violation, fp)
  }

  private fun valid(r: ReproItRect) =
    r.left.isFinite() &&
      r.top.isFinite() &&
      r.right.isFinite() &&
      r.bottom.isFinite() &&
      r.width > 0 &&
      r.height > 0
}

data class ReproItFocusObservation(
  val key: String,
  val focusedEditable: Boolean,
  val field: ReproItRect,
  val usableViewport: ReproItRect,
  val exactKeyboardRect: Boolean,
  val animating: Boolean = false,
  val transformsResolved: Boolean = true,
  val intentionalHiddenEditor: Boolean = false,
  val systemUi: Boolean = false,
)

internal object FocusVisibility {
  private data class C(val sample: () -> ReproItFocusObservation?, val reveal: () -> Boolean)

  private val contracts = linkedMapOf<String, C>()
  private val attempted = hashSetOf<String>()
  private val prior = hashMapOf<String, String>()
  private val counts = hashMapOf<String, Int>()

  @Synchronized
  fun register(id: String, sample: () -> ReproItFocusObservation?, reveal: () -> Boolean) {
    if (id.isNotEmpty()) contracts[id] = C(sample, reveal)
  }

  @Synchronized
  fun clear() {
    contracts.clear()
    attempted.clear()
    prior.clear()
    counts.clear()
  }

  @Synchronized fun hasContracts() = contracts.isNotEmpty()

  @Synchronized
  fun marker(): String? {
    val items = arrayListOf<Map<String, String>>()
    for ((id, c) in contracts.toSortedMap()) {
      val o =
        try {
          c.sample()
        } catch (_: Throwable) {
          null
        }
      if (!valid(o)) {
        reset(id)
        continue
      }
      if (intersects(o!!.field, o.usableViewport)) {
        reset(id)
        continue
      }
      if (!attempted.contains(id)) {
        val safe =
          try {
            c.reveal()
          } catch (_: Throwable) {
            false
          }
        if (!safe) {
          reset(id)
          continue
        }
        attempted.add(id)
        prior.remove(id)
        counts.remove(id)
        continue
      }
      val fp =
        listOf(o.field, o.usableViewport)
          .flatMap { listOf(it.left, it.top, it.width, it.height) }
          .joinToString(",") { (it * 2).roundToInt().toString() }
      val n = if (prior[id] == fp) (counts[id] ?: 0) + 1 else 1
      prior[id] = fp
      counts[id] = n
      if (n >= 2)
        items.add(
          mapOf(
            "id" to "focused-input-obscured:${o.key}",
            "message" to
              "focused editable has no usable visible rectangle after its owning scroll "
                + "container attempted reveal",
          )
        )
    }
    return if (items.isEmpty()) null
    else "REPROIT_INVARIANT " + Json.encode(mapOf("sig" to "", "items" to items))
  }

  private fun reset(id: String) {
    attempted.remove(id)
    prior.remove(id)
    counts.remove(id)
  }

  private fun valid(o: ReproItFocusObservation?) =
    o != null &&
      o.key.isNotEmpty() &&
      o.focusedEditable &&
      o.exactKeyboardRect &&
      !o.animating &&
      o.transformsResolved &&
      !o.intentionalHiddenEditor &&
      !o.systemUi &&
      validRect(o.field) &&
      validRect(o.usableViewport)

  private fun validRect(r: ReproItRect) =
    r.width > 0 && r.height > 0 && listOf(r.left, r.top, r.right, r.bottom).all { it.isFinite() }

  private fun intersects(a: ReproItRect, b: ReproItRect) =
    minOf(a.right, b.right) - maxOf(a.left, b.left) > .5 &&
      minOf(a.bottom, b.bottom) - maxOf(a.top, b.top) > .5
}
