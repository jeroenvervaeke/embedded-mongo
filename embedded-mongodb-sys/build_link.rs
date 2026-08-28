// Putting the resolved library where the linker and, later, the loader will find it.

use std::env;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::Command;

/// On macOS, take ownership of the copy in `OUT_DIR`. ld64 copies `LC_ID_DYLIB` verbatim
/// into every consumer's `LC_LOAD_DYLIB`, so rewriting it to this absolute path is what lets
/// standalone binaries, `cargo test` and maturin's Mach-O repair all resolve it without rpath
/// plumbing in other crates -- which matters, because `cargo:rustc-link-arg` does not reach
/// a crate's dependents. Elsewhere a symlink is enough: the ELF carries no `DT_SONAME`, so
/// consumers record the bare file name and find it through the search path cargo exports.
pub(crate) fn install(source: &Path, destination: &Path, target_os: &str, host: &str) {
    match fs::remove_file(destination) {
        Ok(()) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => panic!("failed to replace {}: {error}", destination.display()),
    }

    if target_os != "macos" {
        #[cfg(unix)]
        std::os::unix::fs::symlink(source, destination)
            .expect("failed to link the native library into OUT_DIR");
        #[cfg(not(unix))]
        fs::copy(source, destination).expect("failed to copy the native library into OUT_DIR");
        return;
    }

    // install_name_tool and codesign are host tools.
    assert!(
        host.contains("apple-darwin"),
        "cross-compiling to {target_os} from {host} needs install_name_tool and codesign, \
         which only exist on macOS. Build on a macOS host, or supply a finished library with \
         EMBEDDED_MONGODB_NATIVE_LIB_DIR."
    );
    fs::copy(source, destination)
        .unwrap_or_else(|error| panic!("failed to copy into {}: {error}", destination.display()));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // Bazel outputs are mode 0555 and fs::copy preserves that, which would stop the next
        // build from replacing this file.
        let _ = fs::set_permissions(destination, fs::Permissions::from_mode(0o755));
    }
    let id = destination.to_str().expect("OUT_DIR must be valid UTF-8");
    run("install_name_tool", &["-id", id, id]);
    // install_name_tool invalidates the linker's ad-hoc signature without replacing it, and
    // an unsigned arm64 dylib is killed at load time.
    run("codesign", &["--force", "--sign", "-", id]);
}

/// Directory holding `libstdc++.a`, or `None` when the toolchain ships no static C++
/// runtime. `rustc` searches its own link paths rather than the compiler's internal library
/// directory, so the location has to be handed to it explicitly. Distributions differ on
/// where it lives -- and some ship only the shared library -- so ask the compiler.
pub(crate) fn static_libstdcxx_dir() -> Option<PathBuf> {
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

fn run(program: &str, args: &[&str]) {
    let status = Command::new(program)
        .args(args)
        .status()
        .unwrap_or_else(|error| panic!("failed to run {program}: {error}"));
    assert!(status.success(), "{program} failed with {status}");
}
