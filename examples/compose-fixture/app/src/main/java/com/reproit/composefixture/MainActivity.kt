package com.reproit.composefixture

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Button
import androidx.compose.material3.Text
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.ExperimentalComposeUiApi
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.testTagsAsResourceId
import androidx.compose.ui.unit.dp
import com.reproit.android.ReproIt
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
    @OptIn(ExperimentalComposeUiApi::class)
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        // Native emulator validation endpoint. Outside a Reproit run the
        // dependency-free client behaves like an ordinary HTTP client; under
        // Appium it emits/replays the universal causal exchange contract.
        thread(name = "reproit-causal-fixture") {
            runCatching { ReproIt.causalHttp.request("http://10.0.2.2:18765/bootstrap") }
        }
        setContent {
            var on by remember { mutableStateOf(false) }
            Column(
                modifier = Modifier
                    .semantics { testTagsAsResourceId = true }
                    .padding(16.dp)
            ) {
                Text(
                    text = if (on) "On" else "Off",
                    modifier = Modifier
                        .testTag("status")
                        .semantics { contentDescription = "status" }
                )
                Button(
                    onClick = { on = !on },
                    modifier = Modifier
                        .testTag("toggle")
                        .semantics { contentDescription = "toggle" }
                ) {
                    Text("Toggle")
                }
                if (on) {
                    Text(
                        text = "Extra panel",
                        modifier = Modifier
                            .testTag("extra")
                            .semantics { contentDescription = "extra" }
                    )
                }
            }
        }
    }
}
