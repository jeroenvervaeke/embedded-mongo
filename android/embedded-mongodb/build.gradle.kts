import org.jetbrains.kotlin.gradle.dsl.JvmTarget

plugins {
    alias(libs.plugins.android.library)
    alias(libs.plugins.kotlin.android)
}

/**
 * Every ABI this library ships, mapped to the Rust target that produces it.
 *
 * Both are 64-bit deliberately, and this map is the only place the set is written down: MongoDB
 * has no 32-bit build, so an `armeabi-v7a` or `x86` install would be a crash on the first query
 * rather than a slower database.
 */
val abiTargets = mapOf(
    "arm64-v8a" to "aarch64-linux-android",
    "x86_64" to "x86_64-linux-android",
)

/** Bionic API level the engine and its prebuilt libraries are compiled against. */
val nativeApiLevel = 24

/**
 * Android 8.0, and deliberately higher than [nativeApiLevel].
 *
 * The engine itself runs on Android 7.0 -- the 23 instrumented tests load it, open a database and
 * answer commands on an API 24 device without a native crash. What does not run there is this
 * module's own public API: `Document` is in every signature, so `org.bson` is an `api` dependency,
 * and `org.bson.conversions.Bson`'s static initializer builds the JSR-310 codecs:
 *
 *     java.lang.NoClassDefFoundError: Failed resolution of: Ljava/time/Instant;
 *         at org.bson.codecs.jsr310.InstantCodec.getEncoderClass(InstantCodec.java:64)
 *         at org.bson.codecs.jsr310.Jsr310CodecProvider.<clinit>(Jsr310CodecProvider.java:44)
 *         at org.bson.conversions.Bson.<clinit>(Bson.java:61)
 *
 * `java.time` arrived in API 26, so on 24 and 25 the first call touching a `Document` dies before
 * it reaches the engine. Core library desugaring would supply `java.time`, but it is not
 * transitive -- an AAR cannot turn it on for the application consuming it -- so 26 is the lowest
 * floor this module can honour on its own.
 */
val minimumSdk = 26

val jniCrate = "embedded-mongodb-android"

android {
    namespace = "io.github.jeroenvervaeke.embeddedmongodb"
    compileSdk = 36

    // The engine needs r27 or newer. Pinned rather than left to AGP's default so that the NDK
    // compiling the bridge is the same one everywhere, and read back by CI, which installs it.
    ndkVersion = "28.2.13676358"

    defaultConfig {
        minSdk = minimumSdk
        ndk { abiFilters += abiTargets.keys }
        consumerProguardFiles("consumer-rules.pro")
        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    lint {
        // A warning worth printing is a warning worth fixing, with three exceptions.
        warningsAsErrors = true
        disable += setOf(
            // Both ask a remote repository whether anything newer was published, so they turn
            // someone else's release into a failing build on a machine that changed nothing.
            "AndroidGradlePluginVersion",
            "NewerVersionAvailable",
            // Reads the ABI list literally and cannot see through abiTargets, so it reports the
            // x86_64 libraries this AAR does ship as missing.
            "ChromeOsAbiSupport",
        )
    }

    sourceSets {
        named("main") { java.srcDirs("src/main/kotlin") }
        named("test") { java.srcDirs("src/test/kotlin") }
        named("androidTest") { java.srcDirs("src/androidTest/kotlin") }
    }
}

kotlin {
    compilerOptions {
        jvmTarget = JvmTarget.JVM_17
        allWarningsAsErrors = true
    }
}

dependencies {
    // Both leak into the public API -- Document in every signature, Flow in one -- so consumers
    // resolve them transitively rather than having to name them again.
    api(libs.bson)
    api(libs.coroutines.core)

    testImplementation(libs.junit)
    testImplementation(libs.kotlin.test)

    androidTestImplementation(libs.androidx.test.junit)
    androidTestImplementation(libs.androidx.test.runner)
    androidTestImplementation(libs.kotlin.test)
}

val cargoJniLibs = tasks.register<CargoJniLibs>("cargoJniLibs") {
    description = "Builds the JNI crate for every supported ABI and stages the libraries the AAR ships."
    group = LifecycleBasePlugin.BUILD_GROUP

    val workspace = rootDir.parentFile
    // The engine is resolved by the sys crate's build script, so its sources decide the contents
    // of the shared library just as much as the JNI crate's own do.
    crateSources.from(
        workspace.resolve("Cargo.toml"),
        workspace.resolve("Cargo.lock"),
        fileTree(workspace.resolve(jniCrate)),
        fileTree(workspace.resolve("embedded-mongodb-sys")),
    )
    manifestPath = workspace.resolve("$jniCrate/Cargo.toml").path
    cargoExecutable = providers.environmentVariable("CARGO").orElse("cargo")
    abis = abiTargets
    apiLevel = nativeApiLevel.toString()
    ndkPath = android.sdkDirectory.resolve("ndk/${android.ndkVersion}").path
    outputDirectory = layout.buildDirectory.dir("jniLibs")
}

androidComponents {
    onVariants { variant ->
        // Registering the directory this way is what carries the task dependency: the AAR cannot
        // be packaged without the libraries, so a stale or missing .so is a build failure.
        variant.sources.jniLibs?.addGeneratedSourceDirectory(cargoJniLibs, CargoJniLibs::outputDirectory)
    }
}
