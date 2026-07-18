package com.reproit.android

enum class ReproItContractStatus {
  VIOLATION,
  SATISFIED,
  ABSTAIN,
}

enum class ReproItStateBoundary(val wire: String) {
  ROTATION("rotation"),
  BACKGROUND_FOREGROUND("background-foreground"),
  NAVIGATION_ROUND_TRIP("navigation-round-trip"),
  PROCESS_RECREATION("process-recreation"),
}

enum class ReproItBoundaryPhase {
  BEFORE,
  AFTER,
}

data class ReproItStructuralObservation(
  val key: String,
  val state: String,
  val authoritative: Boolean,
  val settled: Boolean,
)

data class ReproItContractResult(
  val status: ReproItContractStatus,
  val id: String,
  val message: String? = null,
)

data class ReproItStatePreservationContract(
  val boundaries: Set<ReproItStateBoundary>,
  val sample: () -> ReproItStructuralObservation?,
  val saveBaseline: ((ReproItStateBoundary, ReproItStructuralObservation) -> Boolean)? = null,
  val loadBaseline: ((ReproItStateBoundary) -> ReproItStructuralObservation?)? = null,
)

internal object StatePreservationContracts {
  private val contracts = linkedMapOf<String, ReproItStatePreservationContract>()
  private val baselines = hashMapOf<String, ReproItStructuralObservation>()

  @Synchronized
  fun register(id: String, c: ReproItStatePreservationContract) {
    if (id.isNotEmpty() && c.boundaries.isNotEmpty()) contracts[id] = c
  }

  @Synchronized
  fun clear() {
    contracts.clear()
    baselines.clear()
  }

  @Synchronized
  fun boundary(
    kind: ReproItStateBoundary,
    phase: ReproItBoundaryPhase,
  ): List<ReproItContractResult> {
    val out = arrayListOf<ReproItContractResult>()
    for ((id, c) in contracts.toSortedMap()) {
      if (!c.boundaries.contains(kind)) continue
      val identity = "state-preservation:${kind.wire}:$id"
      val key = "${kind.wire}:$id"
      if (phase == ReproItBoundaryPhase.BEFORE) {
        val value = sampleState(c.sample)
        if (!valid(value)) {
          out += abstain(identity)
          continue
        }
        baselines[key] = value!!
        if (
          kind == ReproItStateBoundary.PROCESS_RECREATION &&
            (c.saveBaseline == null || safeBool { c.saveBaseline.invoke(kind, value) } != true)
        ) {
          baselines.remove(key)
          out += abstain(identity)
        } else out += satisfied(identity)
        continue
      }
      val before =
        if (kind == ReproItStateBoundary.PROCESS_RECREATION)
          c.loadBaseline?.let { sampleState { it(kind) } }
        else baselines[key]
      val after = sampleState(c.sample)
      baselines.remove(key)
      if (!valid(before) || !valid(after)) out += abstain(identity)
      else if (before!!.key == after!!.key && before.state == after.state) out += satisfied(identity)
      else
        out += violation(identity, "declared structural state was not preserved across ${kind.wire}")
    }
    return out
  }
}

data class ReproItActionEffectObservation(
  val route: String? = null,
  val state: String? = null,
  val authoritative: Boolean,
  val settled: Boolean,
)

data class ReproItTargetEffect(val target: String)

data class ReproItChangeEffect(val target: String? = null, val changed: Boolean? = null)

data class ReproItActionEffectContract(
  val sample: () -> ReproItActionEffectObservation?,
  val route: ReproItTargetEffect? = null,
  val state: ReproItChangeEffect? = null,
)

internal object ActionEffectContracts {
  private val contracts = linkedMapOf<String, ReproItActionEffectContract>()
  private val before = hashMapOf<String, ReproItActionEffectObservation>()

  @Synchronized
  fun register(id: String, c: ReproItActionEffectContract) {
    if (id.isNotEmpty()) contracts[id] = c
  }

  @Synchronized
  fun clear() {
    contracts.clear()
    before.clear()
  }

  @Synchronized
  fun begin(id: String): List<ReproItContractResult> {
    val c = contracts[id] ?: return listOf(abstain("action-effect:$id"))
    val value = sampleEffect(c.sample)
    if (!valid(value)) return listOf(abstain("action-effect:$id"))
    before[id] = value!!
    return listOf(satisfied("action-effect:$id"))
  }

  @Synchronized
  fun end(id: String): List<ReproItContractResult> {
    val c = contracts[id]
    val old = before.remove(id)
    val now = c?.let { sampleEffect(it.sample) }
    if (c == null || !valid(old) || !valid(now)) return listOf(abstain("action-effect:$id"))
    val out = arrayListOf<ReproItContractResult>()
    c.route?.let { expected -> checkTarget(out, id, "route", expected.target, now!!.route) }
    c.state?.let { expected -> checkChange(out, id, "state", expected, old!!.state, now!!.state) }
    return if (out.isEmpty()) listOf(abstain("action-effect:$id")) else out
  }
}

internal fun contractMarker(results: List<ReproItContractResult>): String? {
  val items =
    results
      .filter { it.status == ReproItContractStatus.VIOLATION }
      .map { mapOf("id" to it.id, "message" to (it.message ?: it.id)) }
  return if (items.isEmpty()) null
  else "REPROIT_INVARIANT " + Json.encode(mapOf("sig" to "", "items" to items))
}

private fun sampleState(f: () -> ReproItStructuralObservation?) =
  try {
    f()
  } catch (_: Throwable) {
    null
  }

private fun sampleEffect(f: () -> ReproItActionEffectObservation?) =
  try {
    f()
  } catch (_: Throwable) {
    null
  }

private fun valid(o: ReproItStructuralObservation?) =
  o != null && o.authoritative && o.settled && o.key.isNotEmpty() && o.state.isNotEmpty()

private fun valid(o: ReproItActionEffectObservation?) = o != null && o.authoritative && o.settled

private fun safeBool(f: () -> Boolean) =
  try {
    f()
  } catch (_: Throwable) {
    false
  }

private fun abstain(id: String) = ReproItContractResult(ReproItContractStatus.ABSTAIN, id)

private fun satisfied(id: String) = ReproItContractResult(ReproItContractStatus.SATISFIED, id)

private fun violation(id: String, message: String) =
  ReproItContractResult(ReproItContractStatus.VIOLATION, id, message)

private fun checkTarget(
  out: MutableList<ReproItContractResult>,
  id: String,
  kind: String,
  target: String,
  after: String?,
) {
  val identity = "action-effect:$id:$kind"
  out +=
    if (target.isEmpty() || after == null) abstain(identity)
    else if (target == after) satisfied(identity)
    else violation(identity, "declared $kind effect did not occur")
}

private fun checkChange(
  out: MutableList<ReproItContractResult>,
  id: String,
  kind: String,
  e: ReproItChangeEffect,
  before: String?,
  after: String?,
) {
  val identity = "action-effect:$id:$kind"
  if (after == null || (e.target == null && (e.changed == null || before == null))) {
    out += abstain(identity)
    return
  }
  val ok = if (e.target != null) after == e.target else (after != before) == e.changed
  out += if (ok) satisfied(identity) else violation(identity, "declared $kind effect did not occur")
}
