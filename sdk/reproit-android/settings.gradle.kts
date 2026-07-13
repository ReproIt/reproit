pluginManagement {
    plugins {
        id("com.android.library") version "8.12.0"
        id("org.jetbrains.kotlin.android") version "2.2.20"
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
