plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

// Version comes from Cargo.toml so the APK always matches the Rust core.
val cargoVersion: String = run {
    val re = Regex("""^version\s*=\s*"([^"]+)"""", RegexOption.MULTILINE)
    val toml = rootProject.file("../Cargo.toml").readText()
    re.find(toml)?.groupValues?.get(1) ?: "0.0.0"
}
val cargoVersionCode: Int = cargoVersion.split(".").take(3).map { it.filter(Char::isDigit).ifEmpty { "0" }.toInt() }
    .let { p -> (p.getOrElse(0) { 0 } * 10000) + (p.getOrElse(1) { 0 } * 100) + p.getOrElse(2) { 0 } }

android {
    namespace = "dev.unishare.app"
    compileSdk = 35

    defaultConfig {
        applicationId = "dev.unishare.app"
        minSdk = 24
        targetSdk = 35
        versionCode = cargoVersionCode
        versionName = cargoVersion
        ndk { abiFilters += listOf("arm64-v8a", "armeabi-v7a", "x86_64") }
    }

    // Release signing: keystore at android/release.keystore, credentials from the environment
    // (UNISHARE_KEYSTORE_PASS / UNISHARE_KEY_ALIAS). build-android.sh --release creates it on first run.
    val ksFile = rootProject.file("release.keystore")
    val ksPass = System.getenv("UNISHARE_KEYSTORE_PASS")
    val ksAlias = System.getenv("UNISHARE_KEY_ALIAS") ?: "unishare"
    val canSign = ksFile.exists() && !ksPass.isNullOrEmpty()

    signingConfigs {
        if (canSign) {
            create("release") {
                storeFile = ksFile
                storePassword = ksPass
                keyAlias = ksAlias
                keyPassword = ksPass
            }
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            if (canSign) signingConfig = signingConfigs.getByName("release")
        }
        debug {
            applicationIdSuffix = ".debug"
            versionNameSuffix = "-debug"
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    kotlinOptions { jvmTarget = "17" }

    packaging {
        jniLibs { useLegacyPackaging = false }
    }
}

dependencies {
    implementation("androidx.core:core-ktx:1.15.0")
    implementation("androidx.appcompat:appcompat:1.7.0")
    implementation("androidx.activity:activity-ktx:1.9.3")
    implementation("androidx.webkit:webkit:1.12.1")
}
