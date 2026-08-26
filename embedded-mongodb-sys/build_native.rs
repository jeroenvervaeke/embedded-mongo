// Everything that decides what the native library contains. Split out from build.rs and
// `include!`d by it, so that the staleness check in build.rs can watch exactly the inputs
// the published library was built from: a fix to the downloader must not invalidate every
// library already published, and a change to a Bazel flag must.

fn build_native(workspace_root: &Path, crate_root: &Path) -> PathBuf {
    let mongo_root = workspace_root.join("mongo");
    assert!(
        mongo_root.join(".bazelversion").is_file(),
        "MongoDB submodule is missing; run `git submodule update --init --depth 1`"
    );

    let bazel = env::var_os("BAZEL").unwrap_or_else(|| "bazel".into());
    let jobs = env::var("EMBEDDED_MONGODB_BAZEL_JOBS").unwrap_or_else(|_| "8".into());
    assert!(
        jobs.parse::<usize>().is_ok_and(|jobs| jobs > 0),
        "EMBEDDED_MONGODB_BAZEL_JOBS must be a positive integer"
    );
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    // `mongo/.bazelrc` makes `compiler_type` a command-line flag alias, so the value passed
    // below outranks `common:macos --//bazel/config:compiler_type=clang`. Falling back to
    // "gcc" unconditionally therefore fed GCC-only flags into Apple clang.
    let default_compiler_type = if target_os == "macos" { "clang" } else { "gcc" };
    let compiler_type = env::var("CXX")
        .or_else(|_| env::var("CC"))
        .map(|compiler| {
            if compiler.contains("clang") {
                "clang"
            } else {
                "gcc"
            }
        })
        .unwrap_or(default_compiler_type);
    let release = env::var("PROFILE").is_ok_and(|profile| profile == "release");

    eprintln!("building embedded MongoDB with Bazel");
    let mut command = Command::new(&bazel);
    command
        .current_dir(&mongo_root)
        .arg("build")
        .arg("@mongot_localdev//:libembedded_mongodb_native.so")
        .arg(format!(
            "--override_repository=mongot_localdev={}",
            crate_root.join("native").display()
        ))
        // An in-process engine has no network, so the TLS stack and the gRPC/protobuf tree
        // behind it are dead weight, as are OpenTelemetry export and the enterprise-only
        // modules. These pick which engine gets built rather than how well it is optimized,
        // so they apply to every profile: a debug build is for reproducing a release bug,
        // which only works if both link the same engine.
        .args([
            "--//bazel/config:ssl=False",
            "--//bazel/config:build_otel=False",
            "--//bazel/config:build_enterprise=False",
            // Symbolized backtraces, which this library has no way to surface: it exposes
            // five extern "C" entry points and no crash reporter. Turning them off drops
            // cpptrace and libdwarf, the only two linked components MongoDB's own
            // THIRD-PARTY-NOTICES does not cover -- their table marks both as not
            // distributed in release binaries. libdwarf is LGPL-2.1, whose relink
            // obligation a stripped, LTO'd, version-scripted static library cannot
            // discharge. Upstream supports this switch and uses it themselves in
            // `common:remote_unittest`; every call site is behind
            // `#ifdef MONGO_CONFIG_DEV_STACKTRACE`, which the same config sets.
            "--//bazel/config:dev_stacktrace=False",
        ]);
    if release {
        // MongoDB marks nearly every cc_library `alwayslink`, so the linker is handed every
        // object and can only drop whole sections it proves unreachable. Per-function and
        // per-data sections give it that granularity, and --gc-sections then removes the
        // server code this library never reaches.
        command.args([
            "--config=opt",
            // Size, not speed, is the binding constraint on a library that ships inside
            // someone else's application bundle.
            "--//bazel/config:opt=size",
            "--fission=no",
            "--debug_symbols=False",
            "--copt=-fvisibility=hidden",
            "--copt=-ffunction-sections",
            "--copt=-fdata-sections",
        ]);
        match target_os.as_str() {
            "linux" => command.args([
                // Optimizing across the whole engine at link time is worth 7.4 MB. GCC puts
                // its IR in the object files and only a linker carrying GCC's plugin can
                // read it: lld has no such plugin, and handed these objects it links a
                // 3.9 KB library and reports success. The toolchain appends -fuse-ld=lld
                // after the linkopts below, so its features have to be switched off rather
                // than overridden.
                "--copt=-flto",
                "--linkopt=-flto=4",
                "--linkopt=-Os",
                "--linkopt=-fuse-ld=bfd",
                "--features=-linker_lld",
                "--features=-default_linker_lld",
                // bfd does not understand --start-lib.
                "--features=-supports_start_end_lib",
                "--linkopt=-Wl,-z,defs,--strip-all",
                "--linkopt=-Wl,--gc-sections",
                // No --icf=all here: bfd has no identical code folding. It was worth 2.6 MB
                // against lld before LTO, but nothing after it -- linking these objects with
                // mold, which does fold, changes the library by 256 bytes. GCC's -fipa-icf
                // has already found all of it.
                //
                // DT_RELR packs the millions of relative relocations a PIC library of this
                // size accumulates; needs glibc 2.36 or newer at runtime.
                "--linkopt=-Wl,-z,pack-relative-relocs",
                // The published library is built by whichever toolchain CI happens to run,
                // so a dynamic libstdc++ would let its GLIBCXX and CXXABI floor move with
                // the runner image -- and GCC 14 already emits the highest versions
                // manylinux_2_39 permits, leaving no headroom. Safe here because export.map
                // limits the surface to five extern "C" entry points, so no C++ type or
                // exception crosses the boundary. Costs about 2.4 MB.
                "--linkopt=-static-libstdc++",
                "--linkopt=-static-libgcc",
            ]),
            "macos" => command.args([
                "--linkopt=-Wl,-x",
                "--linkopt=-Wl,-dead_strip",
                // Bazel passes plain -shared with no -install_name, which leaves the raw
                // relative bazel-out path in LC_ID_DYLIB -- and ld64 copies that string
                // verbatim into every consumer's LC_LOAD_DYLIB. A consumer rewrites the id
                // to an absolute path, which is much longer, so the Mach-O header needs
                // slack: without it that rewrite fails with "larger updated load commands
                // do not fit" and the only remedy is relinking the library.
                "--linkopt=-Wl,-install_name,@rpath/libembedded_mongodb_native.so",
                "--linkopt=-Wl,-headerpad_max_install_names",
                // `common:macos -c dbg` in mongo/.bazelrc (SERVER-102959) is activated by
                // --config=opt, which turns on --enable_platform_specific_config. Only a
                // command-line compilation mode outranks an rcfile one.
                "-c",
                "opt",
            ]),
            _ => &mut command,
        };
    }
    command
        .arg("--config=native_toolchain")
        .arg(format!("--compiler_type={compiler_type}"))
        .arg(format!("--local_resources=cpu={jobs}"))
        // Python loads extension modules after process startup. TCMalloc's static TLS cannot
        // reliably be allocated that late, so the shared library must use the system allocator.
        .arg("--//bazel/config:allocator=system")
        .arg("--disable_warnings_as_errors=True")
        // mongo/.bazelrc turns on --experimental_collect_system_network_usage, whose
        // collector thread calls sysctl through JNI on every sample. On a memory-tight macOS
        // runner that native call fails and takes the whole Bazel server with it:
        //
        //   FATAL: bazel ran out of memory and crashed
        //   java.lang.OutOfMemoryError: sysctl (Cannot allocate memory)
        //       at SystemNetworkStats.getNetIoCountersNative(Native Method)
        //
        // It collects profiling data nobody reads here, so switch it off everywhere rather
        // than only on the platform that has been seen to die from it.
        .arg("--noexperimental_collect_system_network_usage")
        .arg("--copt=-include")
        .arg("--copt=sys/syscall.h")
        .arg("--copt=-fPIC");
    if target_os == "macos" {
        // --config=native_toolchain sets --linker=lld, and CHOSEN_LINKER in
        // mongo/bazel/toolchains/cc/mongo_native/mongo_native_toolchain.BUILD.tmpl has no
        // //conditions:default arm. On macOS both linker_lld_valid_settings and
        // linker_mold_valid_settings require not_macos, and linker_default requires
        // linker=auto -- so lld matches nothing and Bazel fails during analysis. A
        // command-line --linker outranks the one the config supplies.
        command.arg("--linker=auto");
    }
    let status = command.status().unwrap_or_else(|error| {
        panic!(
            "failed to run {}: {error}; install Bazel with \
             `python3.13 mongo/buildscripts/install_bazel.py` or set BAZEL",
            PathBuf::from(&bazel).display()
        )
    });
    assert!(status.success(), "Bazel native build failed with {status}");

    mongo_root
        .join("bazel-bin/external/mongot_localdev")
        .join(NATIVE_LIBRARY)
}
