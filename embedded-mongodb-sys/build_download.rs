// Getting the published library onto this machine. Everything here is about the network and
// the file system; whether the bytes that arrive may be used at all is `build_freshness`.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::build_cache::{Problem, cache_dir, sweep_stale_temporaries, verify};
use crate::{BASE_URL, NATIVE_LIBRARY, Prebuilt, RELEASE_TAG, flag};

/// Returns a cached, checksum-verified copy of the published library, downloading it first
/// if it is not already there.
pub(crate) fn fetch_prebuilt(entry: &Prebuilt) -> PathBuf {
    // Flat and content-addressed, so two builds wanting the same bytes converge on one path
    // and a manifest bump can never collide with the entry it replaces. Not under the target
    // directory: `cargo clean` must not discard a 33 MB download.
    let cache = cache_dir();
    let destination = cache.join(format!("{}.so", entry.sha256));
    println!("cargo:rerun-if-changed={}", destination.display());
    match verify(&destination, entry) {
        None => return destination,
        Some(Problem::Missing) => {}
        // Truncated by a full disk, or otherwise corrupted. Drop it and fetch again.
        Some(_) => {
            let _ = fs::remove_file(&destination);
        }
    }

    let url = format!("{BASE_URL}/{RELEASE_TAG}/{}", entry.asset);
    if flag("CARGO_NET_OFFLINE") {
        panic!(
            "the prebuilt embedded MongoDB library for `{}` is not in the local cache and \
             CARGO_NET_OFFLINE is set.\n\n  \
             expected at:    {}\n  would download: {url}\n\n  \
             * seed the cache on a networked machine, or point \
             EMBEDDED_MONGODB_NATIVE_LIB_DIR at a directory holding {NATIVE_LIBRARY}\n  \
             * or build the engine yourself: EMBEDDED_MONGODB_BUILD_FROM_SOURCE=1\n",
            entry.target,
            destination.display()
        );
    }

    fs::create_dir_all(&cache)
        .unwrap_or_else(|error| panic!("failed to create {}: {error}", cache.display()));
    sweep_stale_temporaries(&cache);

    // `cargo build --offline` does not export CARGO_NET_OFFLINE to build scripts, so the
    // flag form cannot be detected here. Say what is about to happen rather than reaching
    // the network silently.
    println!("cargo:warning=embedded-mongodb: downloading the prebuilt engine from {url}");

    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.subsec_nanos())
        .unwrap_or_default();
    // In the destination directory, so the rename below is same-filesystem and atomic. No
    // lock file: the worst case for two cold builds racing is one duplicated download.
    let temporary = cache.join(format!(".tmp-{}-{nanos}", std::process::id()));
    if let Err(error) = fetch(&url, &temporary) {
        let _ = fs::remove_file(&temporary);
        panic!(
            "failed to download the prebuilt embedded MongoDB library: {error}\n  url: {url}\n\n  \
             * build the engine yourself: EMBEDDED_MONGODB_BUILD_FROM_SOURCE=1 cargo build\n  \
             * or supply one you have:    EMBEDDED_MONGODB_NATIVE_LIB_DIR=<dir>\n"
        );
    }

    // The two failures get different messages on purpose. curl does not retry a mid-transfer
    // break, so a truncated download is the common one and must not wear the alarming text.
    match verify(&temporary, entry) {
        None => {}
        Some(Problem::Missing) => panic!("{} vanished after download", temporary.display()),
        Some(Problem::Size { actual }) => {
            let _ = fs::remove_file(&temporary);
            panic!(
                "the download was truncated: got {actual} of {} bytes from {url}.\n  \
                 re-run the build to retry.\n",
                entry.size
            );
        }
        Some(Problem::Digest { actual }) => {
            let _ = fs::remove_file(&temporary);
            panic!(
                "checksum mismatch for the prebuilt embedded MongoDB library.\n  url:      \
                 {url}\n  expected: {}\n  actual:   {actual}\n\n  \
                 The file is the expected size but not the expected bytes, so the release \
                 asset may have been replaced. It has not been used.\n",
                entry.sha256
            );
        }
    }

    if fs::rename(&temporary, &destination).is_err() {
        // A racing build may have installed identical bytes already, and an entry that
        // verifies is the entry we wanted either way.
        let installed = verify(&destination, entry).is_none();
        let _ = fs::remove_file(&temporary);
        assert!(installed, "failed to install {}", destination.display());
    }
    destination
}

/// curl first, wget only as a fallback for images that ship one and not the other. No new
/// crates, and curl uses the system trust store, which a TLS-inspecting proxy needs.
fn fetch(url: &str, destination: &Path) -> Result<(), String> {
    let mut last = String::from("neither curl nor wget is installed");
    for name in ["curl", "wget"] {
        let mut command = Command::new(name);
        if name == "wget" {
            command.args(["--quiet", "--timeout=60", "--tries=3", "--output-document"]);
        } else {
            command.args([
                "--silent",
                "--show-error",
                "--location",
                "--fail",
                // Refuses a redirect down to plain http.
                "--proto",
                "=https",
                "--tlsv1.2",
                "--retry",
                "3",
                "--retry-connrefused",
                "--connect-timeout",
                "30",
                "--max-time",
                "1800",
                "--output",
            ]);
        }
        command.arg(destination).arg(url);
        match command.status() {
            Ok(status) if status.success() => return Ok(()),
            // It ran and failed -- 404, TLS, proxy. Report that rather than hiding it behind
            // the next downloader's error.
            Ok(status) => return Err(format!("{name} exited with {status}")),
            Err(error) if error.kind() == ErrorKind::NotFound => {
                last = format!("{name} is not installed");
            }
            Err(error) => return Err(format!("failed to run {name}: {error}")),
        }
    }
    Err(last)
}
