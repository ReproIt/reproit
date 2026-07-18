package com.reproit.android

import org.junit.Assert.*
import org.junit.Test

class IndicatorRelationTest {
  @Test
  fun focusedInputRequiresRevealAndTwoStableHiddenSamples() {
    FocusVisibility.clear()
    var reveals = 0
    FocusVisibility.register(
      "email",
      {
        ReproItFocusObservation(
          "key:email",
          true,
          ReproItRect(0.0, 700.0, 100.0, 740.0),
          ReproItRect(0.0, 0.0, 390.0, 500.0),
          true,
        )
      },
      {
        reveals++
        true
      },
    )
    assertNull(FocusVisibility.marker())
    assertEquals(1, reveals)
    assertNull(FocusVisibility.marker())
    assertTrue(FocusVisibility.marker()!!.contains("focused-input-obscured:key:email"))
    FocusVisibility.clear()
  }

  @Test
  fun requiresStableSamplesAndAbstainsForAnimation() {
    IndicatorRelations.clear()
    var moving = false
    IndicatorRelations.register("liked", "key:badge", "key:liked", "key:tabs", 8.0) {
      ReproItIndicatorGeometry(
        ReproItRect(180.0, 800.0, 190.0, 810.0),
        ReproItRect(160.0, 700.0, 220.0, 750.0),
        ReproItRect(0.0, 680.0, 390.0, 780.0),
        animating = moving,
      )
    }
    assertNull(IndicatorRelations.marker())
    assertTrue(IndicatorRelations.marker()!!.contains("escaped-container"))
    moving = true
    assertNull(IndicatorRelations.marker())
    assertTrue(IndicatorRelations.marker()!!.contains("ABSTAIN"))
    IndicatorRelations.clear()
  }
}
