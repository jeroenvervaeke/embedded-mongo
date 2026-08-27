//! Which native libraries the JVM tests run against, and whether they are this tree's code.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// The probe always, and the shipped library as well when a `cargo build` has left a current
/// one beside it.
///
/// The freshness check is the point: `cargo test` rebuilds the probe but not the package's
/// own `cdylib`, so an unguarded `is_file()` would happily run the harness against whatever
/// the last `cargo build` produced and report a pass -- or a failure -- about code that is no
/// longer in the tree. It is compared against the sources rather than against the probe,
/// because which of the two cargo relinks first depends on the command that was run.
pub fn bridge_libraries() -> Vec<PathBuf> {
    let mut libraries = vec![probe_library()];
    let shipped = profile_dir().join("libembedded_mongodb_android.so");
    match (modified(&shipped), newest_source()) {
        (Some(built), Some(edited)) if built >= edited => libraries.push(shipped),
        (Some(_), Some(_)) => println!(
            "skipping {}: older than this crate's sources, so it is not this tree's code. \
             `cargo build` refreshes it.",
            shipped.display()
        ),
        _ => println!("skipping {}: not built", shipped.display()),
    }
    println!("testing: {libraries:?}");
    libraries
}

/// When the code behind these libraries was last edited.
///
/// Covers the sys crate as well: the bridge and the engine linkage it owns are compiled into
/// both libraries, so an edit there dates them just as surely as an edit here does.
fn newest_source() -> Option<SystemTime> {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace = crate_root.parent()?.to_path_buf();
    [
        crate_root.join("src"),
        crate_root.join("Cargo.toml"),
        workspace.join("embedded-mongodb-sys"),
    ]
    .iter()
    .filter_map(|root| newest_under(root))
    .max()
}

/// Recursive, so that a subdirectory added under `src` later does not silently stop being
/// watched. A directory's own timestamp counts too: that is what moves when a file is added
/// or deleted rather than edited.
fn newest_under(path: &Path) -> Option<SystemTime> {
    let own = modified(path);
    if !path.is_dir() {
        return own;
    }
    let children = std::fs::read_dir(path)
        .ok()?
        .filter_map(Result::ok)
        .filter_map(|entry| newest_under(&entry.path()))
        .max();
    own.max(children)
}

fn modified(path: &Path) -> Option<SystemTime> {
    path.metadata().and_then(|data| data.modified()).ok()
}

/// The library every run of these tests depends on, checked to be this tree's code.
///
/// An assert rather than a skip, and rather than the mere `is_file()` this used to do.
/// `cargo test --test jvm` relinks neither library, so a stale probe would run the harness
/// against code that is no longer in the tree and report a pass -- indistinguishable from a
/// real one, which is exactly how a fixed bug went on looking fixed here. Since this is the
/// only library the tests are guaranteed to have, a stale one makes the whole run worthless.
pub fn probe_library() -> PathBuf {
    let probe = profile_dir().join("examples").join("libjni_probe.so");
    assert!(
        probe.is_file(),
        "{} was not built. `cargo test -p embedded-mongodb-android` and \
         `cargo build --examples` both produce it.",
        probe.display()
    );
    let (Some(built), Some(edited)) = (modified(&probe), newest_source()) else {
        panic!(
            "cannot tell whether {} was built from this tree's sources",
            probe.display()
        );
    };
    assert!(
        built >= edited,
        "{} is older than the sources it should have been built from, so these tests would \
         be checking code that is no longer in the tree. Relink it with \
         `cargo test -p embedded-mongodb-android` or `cargo build --examples`.",
        probe.display()
    );
    probe
}

/// The directory cargo put this test binary's libraries in: `target/<profile>`, or
/// `target/<triple>/<profile>` for a cross build.
pub fn profile_dir() -> PathBuf {
    let executable = std::env::current_exe().expect("a test binary has a path");
    let Some(directory) = executable.parent().and_then(Path::parent) else {
        panic!(
            "{} is not under target/<profile>/deps",
            executable.display()
        );
    };
    directory.to_path_buf()
}

/// Where the JVM will find `libembedded_mongodb_native.so`, which the bindings link against.
///
/// Cargo puts it in the sys crate's `OUT_DIR` and adds that to this process's search path; the
/// child is given it explicitly rather than by inheritance, so that a `java` reached through a
/// wrapper script still finds it.
pub fn library_path() -> OsString {
    let builds = profile_dir().join("build");
    let entries = std::fs::read_dir(&builds).expect("the build directory must be readable");
    let mut newest: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in entries.filter_map(Result::ok) {
        let directory = entry.path().join("out");
        let Ok(modified) = directory
            .join("libembedded_mongodb_native.so")
            .metadata()
            .and_then(|data| data.modified())
        else {
            continue;
        };
        if newest.as_ref().is_none_or(|(seen, _)| *seen < modified) {
            newest = Some((modified, directory));
        }
    }
    let Some((_, directory)) = newest else {
        panic!(
            "no libembedded_mongodb_native.so under {}",
            builds.display()
        );
    };

    let mut path = OsString::from(directory);
    if let Some(inherited) = std::env::var_os("LD_LIBRARY_PATH") {
        path.push(":");
        path.push(inherited);
    }
    path
}
