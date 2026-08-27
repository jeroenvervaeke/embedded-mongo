plugins {
    `kotlin-dsl`
}

repositories {
    mavenCentral()
}

dependencies {
    testImplementation("junit:junit:4.13.2")
    testImplementation("org.jetbrains.kotlin:kotlin-test:$embeddedKotlinVersion")
}

// Gradle no longer runs buildSrc's tests as part of a build, and a test that never runs is not a
// test. Every build asks buildSrc for this jar, so hanging them off it runs them whenever the
// code they cover changes, and skips them otherwise.
tasks.named("jar") { finalizedBy(tasks.named("test")) }
