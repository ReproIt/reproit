pluginManagement {
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

rootProject.name = "composefixture"

include(":app")

include(":reproit-android")

project(":reproit-android").projectDir = file("../../sdk/reproit-android")
