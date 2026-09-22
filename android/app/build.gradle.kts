plugins {
  id("com.android.application")
}

android {
  compileSdk = 34
  namespace = "com.uvcweb.app"
  buildToolsVersion = "37.0.0"

  defaultConfig {
    applicationId = "com.uvcweb.app"
    minSdk = 26
    targetSdk = 34
    // CI sets these from the git tag / run number (see .github/workflows/android.yml)
    versionCode = System.getenv("VERSION_CODE")?.toIntOrNull() ?: 1
    versionName = System.getenv("VERSION_NAME") ?: "0.3.0"

    // the CPU types build-rust.sh produces (jniLibs/<abi>/libuvcweb_core.so)
    ndk {
      abiFilters += listOf("arm64-v8a", "armeabi-v7a", "x86_64")
    }
  }

  splits {
    abi {
      isEnable = true
      reset()
      include(
        "arm64-v8a",
        "armeabi-v7a",
        "x86_64"
      )
      isUniversalApk = true
    }
  }

  // Release signing: if KEYSTORE_FILE is set (CI does that from repository secrets) the release build is
  // signed with your own key, so every version can be installed over the previous one.
  // Without it the release build is signed with the debug key: fine for testing.
  signingConfigs {
    create("release") {
      val keystorePath = System.getenv("KEYSTORE_FILE")
      if (!keystorePath.isNullOrEmpty()) {
        storeFile = file(keystorePath)
        storePassword = System.getenv("KEYSTORE_PASSWORD")
        keyAlias = System.getenv("KEY_ALIAS")
        keyPassword = System.getenv("KEY_PASSWORD")
      }
    }
  }

  buildTypes {
    release {
      isMinifyEnabled = false
      signingConfig = if (!System.getenv("KEYSTORE_FILE").isNullOrEmpty()) {
        signingConfigs.getByName("release")
      } else {
        signingConfigs.getByName("debug")
      }
    }
  }

  compileOptions {
    sourceCompatibility = JavaVersion.VERSION_17
    targetCompatibility = JavaVersion.VERSION_17
  }

  lint {
    abortOnError = false
  }
}

kotlin {
  compilerOptions {
    jvmTarget.set(org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_17)
  }
}

// No library dependencies on purpose: plain Android framework classes only.