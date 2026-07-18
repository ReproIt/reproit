package com.reproit.android

import org.junit.Assert.*
import org.junit.Test

class StructuralContractsTest {
  @Test
  fun stateLossNeedsExplicitAuthoritativeBoundary() {
    StatePreservationContracts.clear()
    var state = "draft:present"
    StatePreservationContracts.register(
      "draft",
      ReproItStatePreservationContract(
        setOf(ReproItStateBoundary.ROTATION),
        { ReproItStructuralObservation("checkout", state, true, true) },
      ),
    )
    assertEquals(
      ReproItContractStatus.SATISFIED,
      StatePreservationContracts.boundary(
          ReproItStateBoundary.ROTATION,
          ReproItBoundaryPhase.BEFORE,
        )[0]
        .status,
    )
    state = "draft:empty"
    val result =
      StatePreservationContracts.boundary(
          ReproItStateBoundary.ROTATION,
          ReproItBoundaryPhase.AFTER,
        )[0]
    assertEquals(ReproItContractStatus.VIOLATION, result.status)
    assertEquals("state-preservation:rotation:draft", result.id)
    StatePreservationContracts.clear()
  }

  @Test
  fun actionEffectsUseDeclaredStructuralValues() {
    ActionEffectContracts.clear()
    var o = ReproItActionEffectObservation("cart", "idle", true, true)
    ActionEffectContracts.register(
      "checkout",
      ReproItActionEffectContract(
        { o },
        ReproItTargetEffect("receipt"),
        ReproItChangeEffect(target = "complete"),
      ),
    )
    ActionEffectContracts.begin("checkout")
    o = ReproItActionEffectObservation("cart", "complete", true, true)
    val ids =
      ActionEffectContracts.end("checkout")
        .filter { it.status == ReproItContractStatus.VIOLATION }
        .map { it.id }
    assertEquals(listOf("action-effect:checkout:route"), ids)
    ActionEffectContracts.clear()
  }

  @Test
  fun processRecreationRequiresPersistentCallbacks() {
    StatePreservationContracts.clear()
    var state = "present"
    var saved: ReproItStructuralObservation? = null
    StatePreservationContracts.register(
      "draft",
      ReproItStatePreservationContract(
        setOf(ReproItStateBoundary.PROCESS_RECREATION),
        { ReproItStructuralObservation("checkout", state, true, true) },
        { _, value ->
          saved = value
          true
        },
        { saved },
      ),
    )
    assertEquals(
      ReproItContractStatus.SATISFIED,
      StatePreservationContracts.boundary(
          ReproItStateBoundary.PROCESS_RECREATION,
          ReproItBoundaryPhase.BEFORE,
        )[0]
        .status,
    )
    state = "empty"
    assertEquals(
      ReproItContractStatus.VIOLATION,
      StatePreservationContracts.boundary(
          ReproItStateBoundary.PROCESS_RECREATION,
          ReproItBoundaryPhase.AFTER,
        )[0]
        .status,
    )
    StatePreservationContracts.clear()
  }

  @Test
  fun unknownPlatformStateAbstains() {
    StatePreservationContracts.clear()
    StatePreservationContracts.register(
      "x",
      ReproItStatePreservationContract(
        setOf(ReproItStateBoundary.BACKGROUND_FOREGROUND),
        { ReproItStructuralObservation("x", "a", false, true) },
      ),
    )
    assertEquals(
      ReproItContractStatus.ABSTAIN,
      StatePreservationContracts.boundary(
          ReproItStateBoundary.BACKGROUND_FOREGROUND,
          ReproItBoundaryPhase.BEFORE,
        )[0]
        .status,
    )
    StatePreservationContracts.clear()
  }
}
