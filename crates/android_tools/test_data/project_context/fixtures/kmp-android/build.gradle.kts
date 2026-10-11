plugins {
    kotlin("multiplatform") version "2.2.10"
    id("com.android.library") version "8.10.0"
}
kotlin { androidTarget(); jvm() }
android { namespace = "dev.koda.context.multiplatform"; compileSdk = 36; defaultConfig { minSdk = 26 } }
