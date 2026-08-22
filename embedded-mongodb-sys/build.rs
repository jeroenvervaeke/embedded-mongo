use std::env;
use std::fs;
use std::io::ErrorKind;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::Command;

const NATIVE_LIBRARY: &str = "libembedded_mongodb_native.so";

fn main() {
    let crate_root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let workspace_root = crate_root
        .parent()
        .expect("embedded-mongodb-sys must be inside the workspace");
    let native_library = match env::var_os("EMBEDDED_MONGODB_NATIVE_LIB_DIR") {
        Some(native_dir) => {
            let native_library = PathBuf::from(native_dir).join(NATIVE_LIBRARY);
            println!("cargo:rerun-if-changed={}", native_library.display());
            native_library
        }
        None => build_native(workspace_root, &crate_root),
    };
    assert!(
        native_library.is_file(),
        "native library not found at {}",
        native_library.display()
    );
    let native_library = native_library
        .canonicalize()
        .expect("failed to resolve native library path");

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let runtime_library = out_dir.join(NATIVE_LIBRARY);
    match fs::remove_file(&runtime_library) {
        Ok(()) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => panic!("failed to replace {}: {error}", runtime_library.display()),
    }
    symlink(&native_library, &runtime_library).expect("failed to link native library into OUT_DIR");

    cxx_build::bridge("src/ffi.rs")
        .file("cpp/bridge.cc")
        .include("include")
        .include("native")
        .std("c++20")
        .compile("embedded-mongodb-cxx");

    println!("cargo:rustc-link-search=native={}", out_dir.display());
    println!("cargo:rustc-link-lib=dylib=embedded_mongodb_native");
    println!("cargo:rerun-if-env-changed=EMBEDDED_MONGODB_NATIVE_LIB_DIR");
    println!("cargo:rerun-if-env-changed=BAZEL");
    println!("cargo:rerun-if-env-changed=CC");
    println!("cargo:rerun-if-env-changed=CXX");
    println!("cargo:rerun-if-env-changed=EMBEDDED_MONGODB_BAZEL_JOBS");
    println!("cargo:rerun-if-env-changed=PROFILE");
    println!("cargo:rerun-if-env-changed=CARGO_CFG_TARGET_OS");
    println!("cargo:rerun-if-changed=src/ffi.rs");
    println!("cargo:rerun-if-changed=cpp/bridge.cc");
    println!("cargo:rerun-if-changed=include/embedded-mongodb/bridge.h");
    println!("cargo:rerun-if-changed=native/BUILD.bazel");
    println!("cargo:rerun-if-changed=native/WORKSPACE.bazel");
    println!("cargo:rerun-if-changed=native/embedded_mongodb_native.cpp");
    println!("cargo:rerun-if-changed=native/embedded_mongodb_native.h");
    println!(
        "cargo:rerun-if-changed={}",
        workspace_root.join("mongo/.bazelversion").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        workspace_root.join("mongo/.bazelrc").display()
    );
}

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
    let compiler_type = env::var("CXX")
        .or_else(|_| env::var("CC"))
        .map(|compiler| {
            if compiler.contains("clang") {
                "clang"
            } else {
                "gcc"
            }
        })
        .unwrap_or("gcc");
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
        ));
    if release {
        // MongoDB marks nearly every cc_library `alwayslink`, so the linker is handed every
        // object and can only drop whole sections it proves unreachable. Per-function and
        // per-data sections give it that granularity; --gc-sections then removes the server
        // code this library never reaches, and --icf folds the duplicate template
        // instantiations C++ leaves behind.
        command.args([
            "--config=opt",
            // Size, not speed, is the binding constraint on a library that ships inside
            // someone else's application bundle.
            "--//bazel/config:opt=size",
            // An in-process engine has no network, so the TLS stack and the gRPC/protobuf
            // tree behind it are dead weight, as are OpenTelemetry export and the
            // enterprise-only modules.
            "--//bazel/config:ssl=False",
            "--//bazel/config:build_otel=False",
            "--//bazel/config:build_enterprise=False",
            "--fission=no",
            "--debug_symbols=False",
            "--copt=-fvisibility=hidden",
            "--copt=-ffunction-sections",
            "--copt=-fdata-sections",
        ]);
        match env::var("CARGO_CFG_TARGET_OS").as_deref() {
            Ok("linux") => command.args([
                "--linkopt=-Wl,-z,defs,--strip-all",
                "--linkopt=-Wl,--gc-sections",
                "--linkopt=-Wl,--icf=all",
                // DT_RELR packs the millions of relative relocations a PIC library of this
                // size accumulates; needs glibc 2.36 or newer at runtime.
                "--linkopt=-Wl,-z,pack-relative-relocs",
            ]),
            Ok("macos") => command.args(["--linkopt=-Wl,-x", "--linkopt=-Wl,-dead_strip"]),
            _ => &mut command,
        };
    }
    let status = command
        .arg("--config=native_toolchain")
        .arg(format!("--compiler_type={compiler_type}"))
        .arg(format!("--local_resources=cpu={jobs}"))
        // Python loads extension modules after process startup. TCMalloc's static TLS cannot
        // reliably be allocated that late, so the shared library must use the system allocator.
        .arg("--//bazel/config:allocator=system")
        .arg("--disable_warnings_as_errors=True")
        .arg("--copt=-include")
        .arg("--copt=sys/syscall.h")
        .arg("--copt=-fPIC")
        .status()
        .unwrap_or_else(|error| {
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
