import org.jetbrains.kotlin.gradle.dsl.JvmTarget
import org.jetbrains.kotlin.gradle.tasks.KotlinCompile

// The half of this library that is not Android.
//
// Databases, collections, queries, cursor paging and every command they are built from live here,
// written against the CommandRunner interface rather than against the engine. `:embedded-mongodb`
// supplies the one implementation that reaches the native library and re-exports this module, so
// an application depending on the AAR sees the whole API and never names this module.
//
// Plain Kotlin so the query layer can be compiled, tested and consumed without the Android SDK,
// the NDK or a compiled engine -- `./gradlew :embedded-mongodb-core:test` needs none of them.
plugins {
    alias(libs.plugins.kotlin.jvm)
}

java {
    // 17 rather than whichever JDK Gradle is running on: the AAR compiles at 17, because that is
    // what AGP is built against, and a module it depends on cannot be newer.
    sourceCompatibility = JavaVersion.VERSION_17
    targetCompatibility = JavaVersion.VERSION_17
}

kotlin {
    compilerOptions {
        jvmTarget = JvmTarget.JVM_17
        allWarningsAsErrors = true
    }
}

dependencies {
    // Both leak into the public API -- Bson and Document in every signature, Flow in the
    // cursor-returning ones -- so consumers resolve them transitively rather than naming them.
    api(libs.bson)
    api(libs.coroutines.core)

    testImplementation(libs.junit)
    testImplementation(libs.kotlin.test)
    testImplementation(libs.coroutines.test)
}

// The virtual-time controls in kotlinx-coroutines-test are still marked experimental, and the
// cursor tests use them to pin what a cancelled collector does. Opted in for the tests only, so
// production code still has to say so at the call site.
tasks.named<KotlinCompile>("compileTestKotlin") {
    compilerOptions.optIn.add("kotlinx.coroutines.ExperimentalCoroutinesApi")
}

tasks.test { useJUnit() }
