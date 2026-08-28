// The content-addressed cache the published libraries land in, and the checks that decide
// whether an entry in it is the entry the manifest describes. Separate from the download
// itself: verification runs on every build, a download only on the first.

use std::env;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use sha2::{Digest, Sha256};

use crate::Prebuilt;

pub(crate) enum Problem {
    Missing,
    Size { actual: u64 },
    Digest { actual: String },
}

/// Size first: it is a free `stat`, and it is what separates "truncated, retry" from "wrong
/// bytes, do not use". Runs on every build-script invocation rather than only after a
/// download, so a cache entry a full disk truncated is caught too.
pub(crate) fn verify(path: &Path, entry: &Prebuilt) -> Option<Problem> {
    let Ok(metadata) = fs::metadata(path) else {
        return Some(Problem::Missing);
    };
    if !metadata.is_file() {
        return Some(Problem::Missing);
    }
    if metadata.len() != entry.size {
        return Some(Problem::Size {
            actual: metadata.len(),
        });
    }
    match sha256(path) {
        Ok(digest) if digest == entry.sha256 => None,
        Ok(digest) => Some(Problem::Digest { actual: digest }),
        Err(_) => Some(Problem::Missing),
    }
}

pub(crate) fn cache_dir() -> PathBuf {
    if let Some(dir) = env::var_os("EMBEDDED_MONGODB_CACHE_DIR") {
        return PathBuf::from(dir);
    }
    if let Some(dir) = env::var_os("XDG_CACHE_HOME") {
        return PathBuf::from(dir).join("embedded-mongodb");
    }
    if let Some(home) = env::var_os("HOME") {
        let home = PathBuf::from(home);
        return if cfg!(target_os = "macos") {
            home.join("Library/Caches/embedded-mongodb")
        } else {
            home.join(".cache/embedded-mongodb")
        };
    }
    // Correct, just not shared between target directories.
    PathBuf::from(env::var_os("OUT_DIR").expect("cargo sets OUT_DIR for build scripts"))
        .join("cache")
}

/// A build killed mid-download leaks a 33 MB temporary permanently; nothing else removes it.
pub(crate) fn sweep_stale_temporaries(cache: &Path) {
    let Ok(entries) = fs::read_dir(cache) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let temporary = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with(".tmp-"));
        if !temporary {
            continue;
        }
        let stale = entry
            .metadata()
            .ok()
            .and_then(|metadata| metadata.modified().ok())
            .and_then(|modified| SystemTime::now().duration_since(modified).ok())
            .is_some_and(|age| age > Duration::from_secs(24 * 60 * 60));
        if stale {
            let _ = fs::remove_file(&path);
        }
    }
}

fn sha256(path: &Path) -> std::io::Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}
