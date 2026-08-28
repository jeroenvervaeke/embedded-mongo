// Whether the published library will load on this host at all. Separate from the freshness
// checks: those ask whether the library matches the source beside it, this asks whether the
// machine can run it, and the two fail for entirely different reasons.

use std::process::Command;

use crate::Prebuilt;

/// The published Linux library's glibc floor is whatever the machine that built it had.
/// Without this check the build succeeds and the failure lands at load time as
/// `version 'GLIBC_2.39' not found`, which says nothing about where the library came from.
pub(crate) fn check_glibc_floor(entry: &Prebuilt) {
    let Some((need_major, need_minor)) = entry.glibc_min else {
        return;
    };
    let Some((have_major, have_minor)) = host_glibc() else {
        println!(
            "cargo:warning=embedded-mongodb: could not read this host's glibc version; the \
             prebuilt library needs {need_major}.{need_minor} or newer"
        );
        return;
    };
    if (have_major, have_minor) >= (need_major, need_minor) {
        return;
    }
    panic!(
        "the prebuilt embedded MongoDB library for `{}` needs glibc {need_major}.{need_minor} \
         or newer, but this host has {have_major}.{have_minor}.\n\n  \
         * build the engine yourself: EMBEDDED_MONGODB_BUILD_FROM_SOURCE=1 cargo build\n  \
         * or supply one built for this host with EMBEDDED_MONGODB_NATIVE_LIB_DIR\n",
        entry.target
    );
}

fn host_glibc() -> Option<(u32, u32)> {
    let parse = |text: &str| -> Option<(u32, u32)> {
        // "glibc 2.44", or ldd's "ldd (GNU libc) 2.44".
        let version = text.split_whitespace().last()?;
        let mut parts = version.split('.');
        Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?))
    };
    if let Ok(output) = Command::new("getconf").arg("GNU_LIBC_VERSION").output()
        && output.status.success()
        && let Some(version) = parse(String::from_utf8_lossy(&output.stdout).trim())
    {
        return Some(version);
    }
    let output = Command::new("ldd").arg("--version").output().ok()?;
    parse(String::from_utf8_lossy(&output.stdout).lines().next()?)
}
