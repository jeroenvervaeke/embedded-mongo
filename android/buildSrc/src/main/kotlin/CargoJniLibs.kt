import groovy.json.JsonSlurper
import java.io.ByteArrayOutputStream
import java.io.File
import javax.inject.Inject
import org.gradle.api.DefaultTask
import org.gradle.api.file.ConfigurableFileCollection
import org.gradle.api.file.DirectoryProperty
import org.gradle.api.file.FileSystemOperations
import org.gradle.api.provider.MapProperty
import org.gradle.api.provider.Property
import org.gradle.api.tasks.Input
import org.gradle.api.tasks.InputFiles
import org.gradle.api.tasks.OutputDirectory
import org.gradle.api.tasks.PathSensitive
import org.gradle.api.tasks.PathSensitivity
import org.gradle.api.tasks.TaskAction
import org.gradle.process.ExecOperations
import org.gradle.work.DisableCachingByDefault

/**
 * Builds the JNI crate for every ABI the AAR ships and stages the three shared libraries each one
 * needs: the JNI bridge, the MongoDB engine it links against, and the NDK's C++ runtime.
 *
 * Cargo is driven directly rather than through `cargo-ndk`, so building this project needs nothing
 * installed beyond a Rust toolchain and the NDK that Gradle already resolves. The compiler
 * variables below are the ones the repository README documents; `cc`, which the `cxx` bridge runs,
 * carries no NDK of its own and would otherwise look for a `<triple>-clang++` that does not exist.
 */
@DisableCachingByDefault(
    because = "the outputs are ~110 MB of shared libraries per variant; cargo's own incremental " +
        "build is cheaper than moving them in and out of the build cache",
)
abstract class CargoJniLibs : DefaultTask() {
    @get:InputFiles
    @get:PathSensitive(PathSensitivity.RELATIVE)
    abstract val crateSources: ConfigurableFileCollection

    @get:Input
    abstract val manifestPath: Property<String>

    @get:Input
    abstract val cargoExecutable: Property<String>

    /** Android ABI to Rust target triple. */
    @get:Input
    abstract val abis: MapProperty<String, String>

    @get:Input
    abstract val apiLevel: Property<String>

    // Tracked as a path rather than as a directory: the NDK is gigabytes of files that never
    // change within a version, and hashing them on every build would cost more than the build.
    @get:Input
    abstract val ndkPath: Property<String>

    @get:OutputDirectory
    abstract val outputDirectory: DirectoryProperty

    @get:Inject
    abstract val execOperations: ExecOperations

    @get:Inject
    abstract val fileSystem: FileSystemOperations

    @TaskAction
    fun stageLibraries() {
        val manifest = File(manifestPath.get())
        check(manifest.isFile) {
            "there is no Rust crate at $manifest, so lib$JNI_LIBRARY.so cannot be built"
        }
        val toolchain = File(ndkPath.get()).resolve("toolchains/llvm/prebuilt/$HOST_TAG")
        check(toolchain.isDirectory) {
            "the NDK at ${ndkPath.get()} has no toolchain for this host; expected $toolchain"
        }
        val targetDirectory = cargoTargetDirectory(manifest)
        // Rebuilt from scratch so a renamed or dropped library cannot survive in the output.
        fileSystem.delete { delete(outputDirectory) }
        abis.get().forEach { (abi, triple) -> stage(abi, triple, manifest, toolchain, targetDirectory) }
    }

    private fun stage(abi: String, triple: String, manifest: File, toolchain: File, targetDirectory: File) {
        execOperations.exec {
            executable = cargoExecutable.get()
            args("build", "--release", "--target", triple, "--manifest-path", manifest.path)
            environment(toolchainEnvironment(triple, toolchain))
        }
        val libraries = listOf(
            targetDirectory.resolve("$triple/release/lib$JNI_LIBRARY.so"),
            engineLibrary(targetDirectory, triple),
            toolchain.resolve("sysroot/usr/lib/$triple/lib$RUNTIME_LIBRARY.so"),
        )
        libraries.forEach { checkBuiltFor(it, triple) }
        fileSystem.copy {
            from(libraries)
            into(outputDirectory.dir(abi))
        }
    }

    /** The variables `cc`, `link-cplusplus` and cargo need to cross-compile to [triple]. */
    private fun toolchainEnvironment(triple: String, toolchain: File): Map<String, String> {
        val bin = toolchain.resolve("bin")
        val clang = bin.resolve("$triple${apiLevel.get()}-clang").path
        val suffix = triple.replace('-', '_')
        return mapOf(
            "ANDROID_NDK_HOME" to ndkPath.get(),
            "CC_$suffix" to clang,
            "CXX_$suffix" to clang + "++",
            "AR_$suffix" to bin.resolve("llvm-ar").path,
            "CARGO_TARGET_${suffix.uppercase()}_LINKER" to clang,
        )
    }

    /**
     * The engine is a shared library of its own, resolved and cached by the sys crate's build
     * script, so it lands in that script's output directory rather than beside the crate's own
     * artifacts. The directory name carries a hash that changes with the crate's features, and
     * older ones are left behind, so the freshest match wins.
     */
    private fun engineLibrary(targetDirectory: File, triple: String): File {
        val candidates = targetDirectory.resolve("$triple/release/build").listFiles().orEmpty()
            .filter { it.isDirectory && it.name.startsWith("embedded-mongodb-sys-") }
            .map { it.resolve("out/lib$ENGINE_LIBRARY.so") }
            .filter { it.isFile }
        check(candidates.isNotEmpty()) {
            "cargo built lib$JNI_LIBRARY.so for $triple but left no lib$ENGINE_LIBRARY.so in " +
                "$targetDirectory/$triple/release/build"
        }
        return candidates.maxBy { it.lastModified() }
    }

    private fun cargoTargetDirectory(manifest: File): File {
        val metadata = ByteArrayOutputStream()
        // Asking cargo rather than assuming <workspace>/target: a CARGO_TARGET_DIR or a
        // .cargo/config.toml in the environment moves it, and guessing wrong stages a stale library.
        execOperations.exec {
            executable = cargoExecutable.get()
            args("metadata", "--format-version", "1", "--no-deps", "--manifest-path", manifest.path)
            standardOutput = metadata
        }
        val parsed = JsonSlurper().parseText(metadata.toString(Charsets.UTF_8.name())) as Map<*, *>
        return File(parsed["target_directory"] as String)
    }
}

/**
 * Rejects a library that is not a 64-bit ELF built for [triple].
 *
 * The engine has no 32-bit build, and a library staged under the wrong ABI directory installs
 * silently and fails at `System.loadLibrary` on a user's device. Reading three fields of the ELF
 * header turns both into a build failure instead.
 */
internal fun checkBuiltFor(library: File, triple: String) {
    check(library.isFile) { "$library was not produced by the cargo build" }
    val header = library.inputStream().use { it.readNBytes(ELF_HEADER_BYTES) }
    check(
        header.size == ELF_HEADER_BYTES &&
            header[0] == 0x7F.toByte() &&
            String(header, 1, 3, Charsets.US_ASCII) == "ELF",
    ) {
        "${library.name} is not an ELF shared library"
    }
    check(header[EI_CLASS].toInt() == ELF_CLASS_64) {
        "${library.name} is a 32-bit library, and MongoDB has no 32-bit build"
    }
    val machine = (header[E_MACHINE].toInt() and 0xFF) or ((header[E_MACHINE + 1].toInt() and 0xFF) shl 8)
    val expected = machineOf(triple)
    check(machine == expected) {
        "${library.name} was built for machine 0x${machine.toString(16)}, but $triple needs " +
            "0x${expected.toString(16)}"
    }
}

private const val JNI_LIBRARY = "embedded_mongodb_android"
private const val ENGINE_LIBRARY = "embedded_mongodb_native"
private const val RUNTIME_LIBRARY = "c++_shared"

private const val ELF_HEADER_BYTES = 20
private const val EI_CLASS = 4
private const val ELF_CLASS_64 = 2
private const val E_MACHINE = 18

/** The NDK ships no arm64 macOS toolchain; Apple silicon runs the x86_64 one under Rosetta. */
private val HOST_TAG =
    if (System.getProperty("os.name").startsWith("Mac")) "darwin-x86_64" else "linux-x86_64"

private fun machineOf(triple: String): Int = when (val architecture = triple.substringBefore('-')) {
    "aarch64" -> 0xB7
    "x86_64" -> 0x3E
    else -> error("$architecture is not an architecture this library is built for")
}
