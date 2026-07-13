plugins {
    id("com.android.library")
    id("org.jetbrains.kotlin.android")
}

android {
    namespace = "com.reproit.android"
    compileSdk = 34

    defaultConfig {
        minSdk = 21
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    kotlinOptions {
        jvmTarget = "17"
    }

    // The pure-Kotlin core (Signature/Engine/Json) has no android.* imports, so
    // the parity test runs as a plain host JVM unit test under src/test.
    testOptions {
        unitTests.isReturnDefaultValues = true
    }
}

dependencies {
    // Jetpack Compose UI, for reading the Compose semantics tree (the same tree
    // the Appium/UiAutomator2 runner reads) so a Compose UI produces the same
    // structural signature as plain Views. `compileOnly`: apps that use Compose
    // already ship it, and apps that do NOT use Compose add no Compose dependency.
    // `ComposeCapture` probes for the runtime class and no-ops when it is absent
    // (see `ReproIt.composePresent`), so the SDK never forces Compose on a host.
    compileOnly("androidx.compose.ui:ui:1.6.8")

    testImplementation("junit:junit:4.13.2")
}
