#!/usr/bin/env bash
# Generate the throwaway sample app used by the emulator proofs.
#
# It mounts the SDK with captureExchanges on, calls a stub upstream through
# ReproIt.causalHttp, and crashes on the response the upstream returns
# ({"prices": null}, where the null IS the cause). A launch with the
# `drain_only` extra skips the crash, which is what a real user's next session
# looks like and where a spooled capsule ships.
#
#   make-proof-app.sh <workdir>
set -euo pipefail

WORK="${1:?usage: make-proof-app.sh <workdir>}"
SDK_DIR="$(cd "$(dirname "$0")/.." && pwd)"
mkdir -p "$WORK/app/src/main/java/com/reproit/proof"

cat > "$WORK/settings.gradle.kts" <<EOF
pluginManagement {
  plugins {
    id("com.android.application") version "8.7.3"
    id("com.android.library") version "8.7.3"
    id("org.jetbrains.kotlin.android") version "2.0.21"
  }
  repositories { google(); mavenCentral(); gradlePluginPortal() }
}
dependencyResolutionManagement { repositories { google(); mavenCentral() } }
rootProject.name = "reproit-android-proof"
include(":app")
includeBuild("$SDK_DIR") {
  dependencySubstitution {
    substitute(module("com.reproit:reproit-android")).using(project(":"))
  }
}
EOF
: > "$WORK/build.gradle.kts"

cat > "$WORK/app/build.gradle.kts" <<'EOF'
plugins {
  id("com.android.application")
  id("org.jetbrains.kotlin.android")
}
android {
  namespace = "com.reproit.proof"
  compileSdk = 34
  defaultConfig {
    applicationId = "com.reproit.proof"
    minSdk = 24
    targetSdk = 34
    versionCode = 1
    versionName = "1.0"
  }
  compileOptions {
    sourceCompatibility = JavaVersion.VERSION_17
    targetCompatibility = JavaVersion.VERSION_17
  }
}
kotlin { compilerOptions { jvmTarget.set(org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_17) } }
dependencies { implementation("com.reproit:reproit-android:1.0.0") }
EOF

cat > "$WORK/app/src/main/AndroidManifest.xml" <<'EOF'
<?xml version="1.0" encoding="utf-8"?>
<manifest xmlns:android="http://schemas.android.com/apk/res/android">
  <uses-permission android:name="android.permission.INTERNET" />
  <application android:label="ReproItProof" android:usesCleartextTraffic="true"
      android:name=".ProofApp">
    <activity android:name=".MainActivity" android:exported="true">
      <intent-filter>
        <action android:name="android.intent.action.MAIN" />
        <category android:name="android.intent.category.LAUNCHER" />
      </intent-filter>
    </activity>
  </application>
</manifest>
EOF

cat > "$WORK/app/src/main/java/com/reproit/proof/ProofApp.kt" <<'EOF'
package com.reproit.proof

import android.app.Application
import android.util.Log
import com.reproit.android.ReproIt
import com.reproit.android.ReproItConfig

class ProofApp : Application() {
  override fun onCreate() {
    super.onCreate()
    ReproIt.init(
      this,
      ReproItConfig(
        appId = "android-proof",
        endpoint = "http://127.0.0.1:39990",
        apiKey = "pk_live_proof",
        buildVersion = "1.0.0",
        buildCommit = "abc123def456",
        captureExchanges = true,
      ),
    )
    Log.i("ReproItProof", "reproit mounted captureExchanges=true")
  }
}
EOF

cat > "$WORK/app/src/main/java/com/reproit/proof/MainActivity.kt" <<'EOF'
package com.reproit.proof

import android.app.Activity
import android.os.Bundle
import android.util.Log
import android.widget.TextView
import com.reproit.android.ReproIt
import org.json.JSONObject

class MainActivity : Activity() {
  override fun onCreate(savedInstanceState: Bundle?) {
    super.onCreate(savedInstanceState)
    setContentView(TextView(this).apply { text = "reproit proof" })
    // A drain-only launch exercises the spool upload without crashing again,
    // which is what a real user's next session looks like.
    if (intent?.getBooleanExtra("drain_only", false) == true) {
      Log.i("ReproItProof", "drain-only launch")
      return
    }
    Thread {
      try {
        val payload =
          JSONObject(mapOf("symbol" to "ACME", "apiKey" to "sk-live-SHOULD-NOT-LEAK")).toString()
        val res =
          ReproIt.causalHttp.request(
            url = "http://127.0.0.1:39991/prices",
            method = "POST",
            headers = mapOf("content-type" to "application/json"),
            body = payload.toByteArray(),
          )
        val body = String(res.body)
        Log.i("ReproItProof", "upstream status=${res.status} body=$body")
        // The planted defect: prices is null, so asking for the array throws.
        JSONObject(body).getJSONArray("prices")
        Log.i("ReproItProof", "NO CRASH (unexpected)")
      } catch (t: Throwable) {
        Log.e("ReproItProof", "planted failure", t)
        throw RuntimeException(t)
      }
    }
      .start()
  }
}
EOF

cp -r "$SDK_DIR/gradle" "$WORK/" 2>/dev/null || true
cp "$SDK_DIR/gradlew" "$WORK/" 2>/dev/null || true
chmod +x "$WORK/gradlew" 2>/dev/null || true
(cd "$WORK" && ANDROID_HOME="${ANDROID_HOME:-$HOME/Library/Android/sdk}" ./gradlew --quiet :app:assembleDebug)
