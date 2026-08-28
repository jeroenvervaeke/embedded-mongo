use anyhow::{Context, Result, bail};
use std::path::Path;

/// What to do with a directory this benchmark cannot prove it created.
pub enum Unrecognised {
    Refuse,
    Delete,
}

/// Dropped into every data directory this benchmark creates. The directory path comes straight
/// off the command line and is deleted before every run, so a typo would otherwise destroy
/// whatever it happened to name. The engine tolerates an unknown file in its dbpath -- it only
/// looks for its own -- which is verified by every run after the first.
const MARKER: &str = ".places-benchmark";
const MARKER_CONTENTS: &str = "\
Created by `cargo run --release --example places-benchmark`.
This directory is deleted and rebuilt from the seed on every run.
";

/// Clears `data_directory` and marks it as this benchmark's own.
pub fn prepare(data_directory: &Path, unrecognised: Unrecognised) -> Result<()> {
    if data_directory.exists() {
        check_removable(data_directory, unrecognised)?;
        std::fs::remove_dir_all(data_directory)
            .with_context(|| format!("could not clear {}", data_directory.display()))?;
    }
    std::fs::create_dir_all(data_directory)?;
    std::fs::write(data_directory.join(MARKER), MARKER_CONTENTS)?;
    Ok(())
}

fn check_removable(data_directory: &Path, unrecognised: Unrecognised) -> Result<()> {
    if matches!(unrecognised, Unrecognised::Delete) || data_directory.join(MARKER).exists() {
        return Ok(());
    }
    let mut entries = std::fs::read_dir(data_directory)
        .with_context(|| format!("{} is not a readable directory", data_directory.display()))?;
    if entries.next().is_none() {
        return Ok(());
    }
    bail!(
        "{} is not empty and has no {MARKER} marker, so this benchmark did not create it; \
         pass --force to delete it anyway",
        data_directory.display()
    );
}

pub struct Footprint {
    pub total: u64,
    pub journal: u64,
}

/// Splits the on-disk cost of a WiredTiger directory, because the two halves scale completely
/// differently: the tables grow with the data, while the journal is preallocated in fixed
/// 100 MiB files whatever the collection holds. The marker this module wrote is discounted, so
/// the report never charges the engine for the benchmark's own bookkeeping.
pub fn footprint(path: &Path) -> Result<Footprint> {
    let marker = path.join(MARKER);
    let marker_bytes = match marker.metadata() {
        Ok(metadata) => metadata.len(),
        Err(_) => 0,
    };
    let journal_path = path.join("journal");
    let journal = if journal_path.is_dir() {
        directory_bytes(&journal_path)?
    } else {
        0
    };
    Ok(Footprint {
        total: directory_bytes(path)? - marker_bytes,
        journal,
    })
}

fn directory_bytes(path: &Path) -> Result<u64> {
    let mut total = 0;
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let metadata = entry.metadata()?;
        total += if metadata.is_dir() {
            directory_bytes(&entry.path())?
        } else {
            metadata.len()
        };
    }
    Ok(total)
}
