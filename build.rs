use std::env;
use std::path::PathBuf;

fn main() {
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let native_dir = env::var_os("EMBEDDED_MONGODB_NATIVE_LIB_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("mongo/bazel-bin/external/mongot_localdev"));
    let native_library = native_dir.join("libembedded_mongodb_native.so");
    assert!(
        native_library.is_file(),
        "native library not found at {}; build the Bazel target first",
        native_library.display()
    );

    cxx_build::bridge("src/lib.rs")
        .file("src/bridge.cc")
        .include("include")
        .include("native")
        .std("c++20")
        .compile("embedded-mongodb-cxx");

    println!("cargo:rustc-link-search=native={}", native_dir.display());
    println!("cargo:rustc-link-lib=dylib=embedded_mongodb_native");
    println!("cargo:rustc-link-arg=-Wl,-rpath,{}", native_dir.display());
    println!("cargo:rerun-if-env-changed=EMBEDDED_MONGODB_NATIVE_LIB_DIR");
    println!("cargo:rerun-if-changed=src/lib.rs");
    println!("cargo:rerun-if-changed=src/bridge.cc");
    println!("cargo:rerun-if-changed=include/embedded-mongodb/bridge.h");
    println!("cargo:rerun-if-changed=native/embedded_mongodb_native.h");
}
