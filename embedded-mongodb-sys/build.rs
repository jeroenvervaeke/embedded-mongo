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
