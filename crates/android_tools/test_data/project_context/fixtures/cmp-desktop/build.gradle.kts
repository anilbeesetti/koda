plugins {
    kotlin("multiplatform") version "2.2.10"
    id("org.jetbrains.kotlin.plugin.compose") version "2.2.10"
    id("org.jetbrains.compose") version "1.8.2"
}
kotlin { jvm("desktop") }
