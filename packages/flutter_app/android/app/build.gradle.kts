import java.util.Properties

plugins {
    id("com.android.application")
    // The Flutter Gradle Plugin must be applied after the Android and Kotlin Gradle plugins.
    id("dev.flutter.flutter-gradle-plugin")
}

// Sessions Firebase credentials are optional and must belong to sh.paseo.sessions.
if (file("google-services.json").isFile) {
    apply(plugin = "com.google.gms.google-services")
}

val signingProperties = Properties().apply {
    val propertiesFile = rootProject.file("key.properties")
    if (propertiesFile.isFile) propertiesFile.inputStream().use { load(it) }
}
fun signingValue(environment: String, property: String): String? =
    System.getenv(environment)?.takeIf { it.isNotBlank() }
        ?: signingProperties.getProperty(property)?.takeIf { it.isNotBlank() }

val releaseStore = signingValue("ANDROID_KEYSTORE_PATH", "storeFile")
val releaseStorePassword = signingValue("ANDROID_KEYSTORE_PASSWORD", "storePassword")
val releaseAlias = signingValue("ANDROID_KEY_ALIAS", "keyAlias")
val releaseKeyPassword = signingValue("ANDROID_KEY_PASSWORD", "keyPassword")
val signingValues = listOf(releaseStore, releaseStorePassword, releaseAlias, releaseKeyPassword)
require(signingValues.all { it == null } || signingValues.all { it != null }) {
    "Provide all Android release signing values, not a partial signing configuration."
}
val hasReleaseSigning = releaseStore != null
val releaseRequested = gradle.startParameter.taskNames.any { it.contains("release", ignoreCase = true) }
if (releaseRequested && !hasReleaseSigning) {
    require(System.getenv("PASEO_ANDROID_REQUIRE_RELEASE_SIGNING") != "1") {
        "A production Android signing key is required for this release."
    }
    logger.warn("Building a locally installable, debug-signed Sessions release; not for production publishing.")
}

android {
    namespace = "sh.paseo.sessions"
    compileSdk = maxOf(37, flutter.compileSdkVersion)
    compileSdkMinor = 0
    ndkVersion = flutter.ndkVersion

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
        isCoreLibraryDesugaringEnabled = true
    }

    defaultConfig {
        applicationId = "sh.paseo.sessions"
        minSdk = 24
        targetSdk = flutter.targetSdkVersion
        versionCode = maxOf(13000, flutter.versionCode)
        versionName = flutter.versionName
    }

    signingConfigs {
        // Keep local upgrade compatibility with the existing Sessions Android app.
        getByName("debug") {
            storeFile = file("debug.keystore")
            storePassword = "android"
            keyAlias = "androiddebugkey"
            keyPassword = "android"
        }
        if (hasReleaseSigning) {
            create("release") {
                storeFile = rootProject.file(releaseStore!!)
                storePassword = releaseStorePassword
                keyAlias = releaseAlias
                keyPassword = releaseKeyPassword
            }
        }
    }

    buildTypes {
        release {
            signingConfig = signingConfigs.getByName(if (hasReleaseSigning) "release" else "debug")
        }
    }
}

androidComponents.finalizeDsl { extension ->
    // Flutter filters its own binaries, but dependencies can still contribute
    // other ABIs. Do not advertise architectures missing libapp/libflutter.
    val targets = providers.gradleProperty("target-platform").orNull
    if (targets != null && !extension.splits.abi.isEnable) {
        extension.defaultConfig.ndk.abiFilters.apply {
            clear()
            addAll(targets.split(",").map { target ->
                when (target) {
                    "android-arm" -> "armeabi-v7a"
                    "android-arm64" -> "arm64-v8a"
                    "android-x64" -> "x86_64"
                    else -> error("Unsupported Flutter Android target: $target")
                }
            })
        }
    }
}

kotlin {
    compilerOptions {
        jvmTarget = org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_17
    }
}

flutter {
    source = "../.."
}

dependencies {
    coreLibraryDesugaring("com.android.tools:desugar_jdk_libs:2.1.4")
}
