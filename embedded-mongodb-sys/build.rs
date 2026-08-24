use std::env;
use std::fs;
use std::io::ErrorKind;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::Command;

// Everything that decides what the native library contains.
include!("build_native.rs");

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

    // Half of the C++ runtime story; the native library covers the other half through its
    // own link flags. Without this the cxx bridge puts a `NEEDED libstdc++.so.6` on every
    // artifact that links it -- the Python extension module included -- which pins the
    // wheel's GLIBCXX floor to whichever toolchain happened to build it.
    let static_libstdcxx = (env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux")
        && env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("gnu"))
    .then(static_libstdcxx_dir)
    .flatten();
    let mut bridge = cxx_build::bridge("src/ffi.rs");
    bridge
        .file("cpp/bridge.cc")
        .include("include")
        .include("native")
        .std("c++20");
    if static_libstdcxx.is_some() {
        bridge.cpp_link_stdlib(None);
    }
    bridge.compile("embedded-mongodb-cxx");
    if let Some(directory) = static_libstdcxx {
        // Emitted after compile(), so the archive that needs these symbols precedes them on
        // the link line.
        println!("cargo:rustc-link-search=native={}", directory.display());
        println!("cargo:rustc-link-lib=static=stdc++");
    }

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

/// Directory holding `libstdc++.a`, or `None` when the toolchain ships no static C++
/// runtime. `rustc` searches its own link paths rather than the compiler's internal library
/// directory, so the location has to be handed to it explicitly. Distributions differ on
/// where it lives -- and some ship only the shared library -- so ask the compiler.
fn static_libstdcxx_dir() -> Option<PathBuf> {
    let compiler = env::var("CXX").unwrap_or_else(|_| "c++".into());
    let output = Command::new(compiler)
        .arg("-print-file-name=libstdc++.a")
        .output()
        .ok()?;
    // A compiler that cannot find the file echoes the bare name back.
    let path = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
    if !output.status.success() || !path.is_file() {
        println!(
            "cargo:warning=embedded-mongodb: no static libstdc++ found; linking it \
             dynamically, which ties this build to the host's GLIBCXX version"
        );
        return None;
    }
    Some(path.parent()?.to_path_buf())
}
