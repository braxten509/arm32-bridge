// Game Bridge: converts 32-bit-only games from Downloads on the phone itself.
// tools/build-app.sh puts the bridge, the stand-in and the converter into
// src/main/jniLibs/arm64-v8a and the signing key into src/main/assets first.
plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.plugin.compose")
}

android {
    namespace = "dev.braxten.gamebridge"
    compileSdk = 37

    defaultConfig {
        applicationId = "dev.braxten.gamebridge"
        minSdk = 31
        targetSdk = 36
        versionCode = 1
        versionName = "0.1"
        ndk.abiFilters += "arm64-v8a"
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            signingConfig = signingConfigs.getByName("debug")
        }
    }

    buildFeatures { compose = true }

    // The converter runs as a program and the bridge files are copied into games, so they
    // must exist as real files in the app's library folder.
    packaging.jniLibs.useLegacyPackaging = true
}

dependencies {
    implementation(platform("androidx.compose:compose-bom:2026.09.00"))
    implementation("androidx.activity:activity-compose:1.13.0")
    implementation("androidx.compose.material3:material3")
    implementation("androidx.lifecycle:lifecycle-runtime-compose:2.11.0")
    // Signs converted games with the same key as tools/convert.sh.
    implementation("com.android.tools.build:apksig:9.4.1")
}
