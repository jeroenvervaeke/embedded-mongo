fn main() {
    napi_build::setup();

    // The addon records a bare `libembedded_mongodb_native.so` with no path, and the package
    // ships that library beside the addon. Pointing the loader at the addon's own directory is
    // what makes the two find each other wherever npm unpacks them.
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    match target_os.as_str() {
        "linux" => println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN"),
        "macos" => println!("cargo:rustc-link-arg=-Wl,-rpath,@loader_path"),
        _ => {}
    }
}
