plugins {
  id("com.android.library")
  id("org.jetbrains.kotlin.android")
  id("maven-publish")
}

group = "com.reproit"

version = "0.1.0"

android {
  namespace = "com.reproit.android"
  compileSdk = 34

  defaultConfig { minSdk = 21 }

  compileOptions {
    sourceCompatibility = JavaVersion.VERSION_17
    targetCompatibility = JavaVersion.VERSION_17
  }

  // The pure-Kotlin core (Signature/Engine/Json) has no android.* imports, so
  // the parity test runs as a plain host JVM unit test under src/test.
  testOptions { unitTests.isReturnDefaultValues = true }

  publishing { singleVariant("release") { withSourcesJar() } }
}

kotlin { compilerOptions { jvmTarget.set(org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_17) } }

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

afterEvaluate {
  publishing {
    publications {
      create<MavenPublication>("release") {
        from(components["release"])
        groupId = "com.reproit"
        artifactId = "reproit-android"
        version = project.version.toString()
        pom {
          name.set("ReproIt Android")
          description.set(
            "Structural UI telemetry that turns production Android bugs into deterministic replays."
          )
          url.set("https://reproit.com")
          licenses {
            license {
              name.set("Apache License 2.0")
              url.set("https://www.apache.org/licenses/LICENSE-2.0")
            }
          }
          developers { developer { name.set("ReproIt, Inc.") } }
          scm {
            connection.set("scm:git:https://github.com/ReproIt/reproit.git")
            url.set("https://github.com/ReproIt/reproit")
          }
        }
      }
    }
  }
}
