// The Android half of what decides what the native library contains. `include!`d by
// build_native.rs, and watched alongside it, because the NDK it picks and the API level it
// targets are part of the published library's ABI.

/// Android API level the published libraries target. Android 7.0, the oldest release NDK 27
/// and 28 still support, which covers effectively every device that still receives apps.
/// `EMBEDDED_MONGODB_ANDROID_API` overrides it for a source build; a prebuilt is whatever
/// CI compiled.
const ANDROID_API_LEVEL: &str = "24";

/// Where to find an NDK, in the order the Android SDK, Gradle and the GitHub runner images
/// set them.
const ANDROID_NDK_VARIABLES: &[&str] = &["ANDROID_NDK_HOME", "ANDROID_NDK_ROOT", "ANDROID_NDK"];

/// The NDK tools an Android build needs, resolved to absolute paths.
///
/// Every path is absolute and every choice is baked in, because Bazel scrubs the environment
/// for sandboxed actions: a compiler wrapper that read the target triple from a variable at
/// run time would see nothing and silently build for the host instead.
struct AndroidNdk {
    cc: PathBuf,
    cxx: PathBuf,
    bin: PathBuf,
    /// `@platforms//cpu:` value for the target. This is what decides which per-architecture
    /// sources and compiler flags Bazel selects, so on a cross build it must name the target
    /// rather than the machine running the compiler.
    cpu: &'static str,
}

impl AndroidNdk {
    fn detect(target_arch: &str) -> AndroidNdk {
        // No 32-bit Android. MongoDB builds only for 64-bit platforms, and both remaining
        // 32-bit ABIs are below the memory ceiling WiredTiger's cache assumes.
        let (triple, cpu) = match target_arch {
            "aarch64" => ("aarch64-linux-android", "aarch64"),
            "x86_64" => ("x86_64-linux-android", "x86_64"),
            other => panic!(
                "the embedded MongoDB engine has no Android build for `{other}`; \
                 aarch64 and x86_64 are the supported architectures"
            ),
        };

        let root = ANDROID_NDK_VARIABLES
            .iter()
            .find_map(env::var_os)
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                panic!(
                    "building the engine for Android needs the NDK; set one of {} to an \
                     NDK r27 or newer",
                    ANDROID_NDK_VARIABLES.join(", ")
                )
            });
        // Apple silicon hosts run the x86_64 binaries under Rosetta; the NDK ships no arm64
        // macOS variant.
        let host_tag = if cfg!(target_os = "macos") {
            "darwin-x86_64"
        } else {
            "linux-x86_64"
        };
        let bin = root
            .join("toolchains/llvm/prebuilt")
            .join(host_tag)
            .join("bin");

        let api = env::var("EMBEDDED_MONGODB_ANDROID_API")
            .unwrap_or_else(|_| ANDROID_API_LEVEL.to_string());
        assert!(
            api.parse::<u32>().is_ok_and(|api| api >= 21),
            "EMBEDDED_MONGODB_ANDROID_API must be 21 or higher, got `{api}`"
        );
        let cc = bin.join(format!("{triple}{api}-clang"));
        let cxx = bin.join(format!("{triple}{api}-clang++"));
        assert!(
            cc.is_file() && cxx.is_file(),
            "{} does not hold a clang for {triple} at API {api}; check the NDK version and \
             EMBEDDED_MONGODB_ANDROID_API",
            bin.display()
        );

        AndroidNdk { cc, cxx, bin, cpu }
    }

    /// Points Bazel's system-compiler toolchain at the NDK and tells it which architecture it
    /// is now targeting.
    fn apply(&self, command: &mut Command) {
        command
            .env("CC", &self.cc)
            .env("CXX", &self.cxx)
            // Bazel's `as` tool path is only reached for a handful of actions, but the host
            // assembler would mis-assemble every one of them. clang drives the NDK's.
            .env("AS", &self.cc)
            .env("MONGO_NATIVE_TARGET_CPU", self.cpu);
        // The host binutils cannot read objects built for another architecture.
        for (variable, tool) in [
            ("AR", "llvm-ar"),
            ("NM", "llvm-nm"),
            ("LD", "ld.lld"),
            ("DWP", "llvm-dwp"),
            ("OBJCOPY", "llvm-objcopy"),
            ("OBJDUMP", "llvm-objdump"),
            ("STRIP", "llvm-strip"),
        ] {
            command.env(variable, self.bin.join(tool));
        }

        // Bazel resolves per-architecture sources and flags from the target platform, and
        // its default is the host's. Without this an aarch64 build is handed
        // `-march=sandybridge` and the x86_64 libunwind sources.
        command.arg(format!(
            "--platforms=@mongot_localdev//:linux_{}_native",
            self.cpu
        ));
        command.args([
            // clang below 20 predefines neither, so libc++ leaves out
            // std::hardware_{con,de}structive_interference_size -- which mongo uses in
            // several thousand places. 64 is the value clang 20 and GCC give both Android
            // architectures.
            "--cxxopt=-D__GCC_DESTRUCTIVE_SIZE=64",
            "--cxxopt=-D__GCC_CONSTRUCTIVE_SIZE=64",
            // mongo_linux's `global_libs` feature adds -lresolv, which bionic folds into
            // libc and therefore does not ship. The other two libraries it names do exist.
            "--features=-global_libs",
            "--linkopt=-lm",
            "--linkopt=-latomic",
        ]);
    }
}
