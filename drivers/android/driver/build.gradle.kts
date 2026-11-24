import com.google.protobuf.gradle.proto

plugins {
    alias(libs.plugins.android.application)
    alias(libs.plugins.protobuf)
}

// The driver ships in lockstep with the CLI: versionName is the crate version,
// versionCode is derived from it so a newer Tapwing always installs over an
// older driver (a downgrade is handled by the CLI with uninstall + install).
val cargoVersion: String = rootDir.resolve("../../crates/tapwing/Cargo.toml").readLines()
    .first { it.startsWith("version = ") }
    .substringAfter('"').substringBefore('"')
val cargoVersionCode: Int = cargoVersion.substringBefore('-').split('.').map(String::toInt)
    .let { (major, minor, patch) -> major * 1_000_000 + minor * 1_000 + patch }

android {
    namespace = "dev.tapwing.driver"
    compileSdk = 37

    defaultConfig {
        applicationId = "dev.tapwing.driver"
        // Same floor as Maestro 2.12.1's driver.
        minSdk = 24
        targetSdk = 37
        versionCode = cargoVersionCode
        versionName = cargoVersion
    }

    signingConfigs {
        // Intentionally public key, committed on purpose: it only keeps the
        // signature stable so updates install over older drivers. See
        // signing/README.md for why this is not a secret.
        create("tapwingTestDriver") {
            storeFile = rootDir.resolve("signing/tapwing-test-driver.p12")
            storeType = "PKCS12"
            storePassword = "tapwing-test-driver"
            keyAlias = "tapwing-test-driver"
            keyPassword = "tapwing-test-driver"
        }
    }

    buildTypes {
        getByName("release") {
            // grpc and protobuf-lite use reflection; the APK is a test tool
            // installed over adb, so size matters less than robustness.
            isMinifyEnabled = false
            signingConfig = signingConfigs.getByName("tapwingTestDriver")
        }
        getByName("debug") {
            signingConfig = signingConfigs.getByName("tapwingTestDriver")
        }
    }

    sourceSets {
        getByName("main") {
            // Single source of truth for the wire format, shared with the Rust client.
            proto { srcDir(rootDir.resolve("../../crates/tapwing/proto")) }
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    packaging {
        resources {
            excludes += listOf("META-INF/INDEX.LIST", "META-INF/*.version", "META-INF/LICENSE*", "META-INF/NOTICE*")
        }
    }

    dependenciesInfo {
        // The Play Store dependency block is encrypted with a random key, which
        // would make every build's APK (and its SHA-256) different.
        includeInApk = false
        includeInBundle = false
    }

    lint {
        // `ACCESS_MOCK_LOCATION`, `QUERY_ALL_PACKAGES`: a test driver, never on a store.
        disable += listOf("MockLocation", "ProtectedPermissions", "QueryAllPackagesPermission", "ExpiredTargetSdkVersion")
    }
}

protobuf {
    protoc {
        artifact = "com.google.protobuf:protoc:${libs.versions.protobuf.get()}"
    }
    plugins {
        create("grpc") {
            artifact = "io.grpc:protoc-gen-grpc-java:${libs.versions.grpc.get()}"
        }
    }
    generateProtoTasks {
        all().forEach { task ->
            task.builtins {
                create("java") { option("lite") }
            }
            task.plugins {
                create("grpc") { option("lite") }
            }
        }
    }
}

dependencies {
    implementation(libs.grpc.okhttp)
    implementation(libs.grpc.protobuf.lite)
    implementation(libs.grpc.stub)
    implementation(libs.protobuf.javalite)
    implementation(libs.androidx.uiautomator)
    implementation(libs.androidx.test.monitor)
    implementation(libs.gms.location)
    compileOnly(libs.javax.annotation)

    testImplementation(libs.junit)
}
