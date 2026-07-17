package com.reproit.composefixture

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.material3.Button
import androidx.compose.material3.Text
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.ExperimentalComposeUiApi
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Rect
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.layout.boundsInWindow
import androidx.compose.ui.layout.onGloballyPositioned
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.testTagsAsResourceId
import androidx.compose.ui.unit.dp
import com.reproit.android.ReproIt
import com.reproit.android.ReproItActionEffectContract
import com.reproit.android.ReproItActionEffectObservation
import com.reproit.android.ReproItBoundaryPhase
import com.reproit.android.ReproItConfig
import com.reproit.android.ReproItFocusObservation
import com.reproit.android.ReproItIndicatorGeometry
import com.reproit.android.ReproItRect
import com.reproit.android.ReproItStateBoundary
import com.reproit.android.ReproItStatePreservationContract
import com.reproit.android.ReproItStructuralObservation
import com.reproit.android.ReproItTargetEffect
import kotlin.concurrent.thread

// Minimal REAL Jetpack Compose app used to prove reproit drives a native Compose
// UI through Appium/UiAutomator2 (runners/rn/runner.mjs).
//
// A single Button toggles a boolean. Flipping it conditionally emits an extra
// Text into the tree, so the tap moves the app to a STRUCTURALLY different state
// (a text-only change would not move reproit's canonical structural signature).
//
// `testTagsAsResourceId = true` on the root maps every child `Modifier.testTag`
// to an Android resource-id, which the runner reads as a stable `key:` selector;
// each interactive/reported node also carries a semantics contentDescription.
class MainActivity : ComponentActivity() {
  @Volatile private var indicatorRect: Rect? = null
  @Volatile private var ownerRect: Rect? = null
  @Volatile private var containerRect: Rect? = null

  @OptIn(ExperimentalComposeUiApi::class)
  override fun onCreate(savedInstanceState: Bundle?) {
    super.onCreate(savedInstanceState)
    ReproIt.init(application, ReproItConfig(appId = "compose-fixture"))
    ReproIt.indicator("fixture.badge", "key:badge", "key:toggle", "key:screen") {
      val indicator = indicatorRect
      val owner = ownerRect
      val container = containerRect
      if (indicator == null || owner == null || container == null) null
      else ReproItIndicatorGeometry(indicator.toReproIt(), owner.toReproIt(), container.toReproIt())
    }
    ReproIt.focusedInput(
      "fixture.email",
      sample = {
        ReproItFocusObservation(
          "key:email",
          true,
          ReproItRect(20.0, 1800.0, 600.0, 1900.0),
          ReproItRect(0.0, 0.0, 1080.0, 1400.0),
          true,
        )
      },
      reveal = { true },
    )
    var preservedState = "draft:present"
    ReproIt.preserveState(
      "fixture.draft",
      ReproItStatePreservationContract(
        setOf(ReproItStateBoundary.ROTATION),
        { ReproItStructuralObservation("checkout", preservedState, true, true) },
      ),
    )
    ReproIt.stateBoundary(ReproItStateBoundary.ROTATION, ReproItBoundaryPhase.BEFORE)
    preservedState = "draft:empty"
    ReproIt.stateBoundary(ReproItStateBoundary.ROTATION, ReproItBoundaryPhase.AFTER)
    var effect = ReproItActionEffectObservation("cart", "idle", true, true)
    ReproIt.actionEffect(
      "fixture.checkout",
      ReproItActionEffectContract({ effect }, ReproItTargetEffect("receipt")),
    )
    ReproIt.actionBegin("fixture.checkout")
    effect = ReproItActionEffectObservation("cart", "complete", true, true)
    ReproIt.actionEnd("fixture.checkout")
    // Native emulator validation endpoint. Outside a Reproit run the
    // dependency-free client behaves like an ordinary HTTP client; under
    // Appium it emits/replays the universal causal exchange contract.
    thread(name = "reproit-causal-fixture") {
      runCatching { ReproIt.causalHttp.request("http://10.0.2.2:18765/bootstrap") }
    }
    setContent {
      var on by remember { mutableStateOf(false) }
      Column(
        modifier =
          Modifier.fillMaxSize()
            .onGloballyPositioned { containerRect = it.boundsInWindow() }
            .semantics { testTagsAsResourceId = true }
            .padding(16.dp)
      ) {
        Text(
          text = if (on) "On" else "Off",
          modifier = Modifier.testTag("status").semantics { contentDescription = "status" },
        )
        Button(
          onClick = { on = !on },
          modifier =
            Modifier.onGloballyPositioned { ownerRect = it.boundsInWindow() }
              .testTag("toggle")
              .semantics { contentDescription = "toggle" },
        ) {
          Text("Toggle")
        }
        Box(
          modifier =
            Modifier.offset(y = 100.dp)
              .size(10.dp)
              .background(Color.Red)
              .onGloballyPositioned { indicatorRect = it.boundsInWindow() }
              .testTag("badge")
        )
        if (on) {
          Text(
            text = "Extra panel",
            modifier = Modifier.testTag("extra").semantics { contentDescription = "extra" },
          )
        }
      }
    }
  }

  private fun Rect.toReproIt() =
    ReproItRect(left.toDouble(), top.toDouble(), right.toDouble(), bottom.toDouble())
}
