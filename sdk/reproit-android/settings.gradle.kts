// Plugin and Gradle versions are pinned to the SAME pair the composite build
// in fixtures/compose-fixture uses (Gradle 8.11.1 via the committed wrapper,
// AGP 8.7.3, Kotlin 2.0.21). One toolchain per module regardless of entry
// point: a green `./gradlew test` here compiles the same bytes the composite
// and device builds compile, so a standalone pass is evidence about the AAR
// that actually ships.
pluginManagement {
  plugins {
    id("com.android.library") version "8.7.3"
    id("org.jetbrains.kotlin.android") version "2.0.21"
  }
  repositories {
    google()
    mavenCentral()
    gradlePluginPortal()
  }
}

dependencyResolutionManagement {
  repositories {
    google()
    mavenCentral()
  }
}

rootProject.name = "reproit-android"
