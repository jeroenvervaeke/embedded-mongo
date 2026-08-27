use crate::{
    measure::{drain, time},
    queries,
    report::{bytes, millis, row},
    rss::{Sampler, peak_rss},
};
use anyhow::{Context, Result};
use embedded_mongodb::{Client, bson::doc};
use std::{
    path::{Path, PathBuf},
    process::Command,
};

/// Runs [`open`] in a child process, so the measurement sees an engine that has never held
/// this data.
pub fn spawn(data_directory: &Path, repeats: usize) -> Result<()> {
    let status = Command::new(executable()?)
        .arg("--cold-open")
        .arg(data_directory)
        .arg(repeats.to_string())
        .status()?;
    if !status.success() {
        anyhow::bail!("the cold-open child exited with {status}");
    }
    Ok(())
}

/// `current_exe` reads `/proc/self/exe`, which still points at the old inode after a rebuild
/// replaces the binary and comes back with a " (deleted)" marker glued on the path. Strip it,
/// so a rebuild landing mid-run costs the cold-open measurement nothing.
fn executable() -> Result<PathBuf> {
    let path = std::env::current_exe()?;
    if path.exists() {
        return Ok(path);
    }
    let replacement = path
        .to_string_lossy()
        .strip_suffix(" (deleted)")
        .map(PathBuf::from)
        .filter(|replacement| replacement.exists());
    replacement.with_context(|| format!("{} no longer exists", path.display()))
}

/// Opens an already-populated directory and runs the app's nearby search, timing the whole
/// path from `Client::new` to the first result.
///
/// The timed query is the `$geoWithin` scan rather than `$geoNear`, because reopening a data
/// directory restores no index entries into the in-memory catalog -- `listIndexes` still
/// reports every index, but `collStats.nindexes` is zero and the planner cannot see them. The
/// two rows after the timing record that, since it decides whether the demo app can persist
/// its database at all.
pub fn open(data_directory: &Path, repeats: usize) -> Result<()> {
    let sampler = Sampler::start();
    let ((client, matches), elapsed) = time(|| {
        let client = Client::new(data_directory)?;
        let matches = drain(queries::collection(&client).find(queries::geo_within())?)?;
        Ok((client, matches))
    })?;

    // Repeating the search shows whether serving queries settles at the cache size or keeps
    // growing, which is the difference between an app that runs all day and one that is killed.
    let places = queries::collection(&client);
    for _ in 1..repeats {
        drain(places.find(queries::geo_within())?)?;
    }

    row("open to first $geoWithin result", millis(elapsed));
    row("documents returned", matches.to_string());
    row("$geoWithin repeats", repeats.to_string());
    row(
        "peak RSS of the cold process",
        format!(
            "{} sampled, {} VmHWM",
            bytes(sampler.take_peak()),
            bytes(peak_rss()?)
        ),
    );

    let indexes = client
        .database(queries::DATABASE)
        .run_command(&doc! { "collStats": queries::COLLECTION })?
        .get_i32("nindexes")
        .unwrap_or(-1);
    row("indexes visible to the planner", indexes.to_string());
    let near = queries::collection(&client).aggregate(queries::geo_near(None));
    row(
        "$geoNear on the reopened directory",
        match near.map_err(anyhow::Error::from).and_then(drain) {
            Ok(found) => format!("{found} docs"),
            Err(error) => error.to_string(),
        },
    );
    client.close()?;
    Ok(())
}
