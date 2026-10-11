// JVM smoke test for the generated Kotlin bindings: compiles them unchanged
// in a Kotlin Multiplatform jvm() target (how the apps' SDKs consume them)
// and drives a real server through JNA. Run via ../scripts/kotlin-smoke.sh.
plugins {
    kotlin("multiplatform") version "2.4.20"
}

repositories { mavenCentral() }

kotlin {
    jvm()
    sourceSets {
        jvmMain {
            kotlin.srcDir("../bindings/kotlin")
            dependencies {
                implementation("net.java.dev.jna:jna:5.19.1")
                implementation("org.jetbrains.kotlinx:kotlinx-coroutines-core:1.9.0")
            }
        }
    }
}

tasks.register<JavaExec>("smoke") {
    val main = kotlin.jvm().compilations["main"]
    dependsOn(main.compileTaskProvider)
    classpath = main.output.allOutputs + main.runtimeDependencyFiles!!
    mainClass.set("SmokeKt")
    // The host library's directory, passed by ../scripts/kotlin-smoke.sh.
    val jnaPath = project.findProperty("jnaLibraryPath") as String?
        ?: "${rootDir}/../../../target/release"
    jvmArgs("-Djna.library.path=$jnaPath")
    args((project.findProperty("smokeArgs") as String? ?: "").split(" ").filter { it.isNotBlank() })
}
