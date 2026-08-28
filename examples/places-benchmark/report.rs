use crate::{
    datadir::Footprint,
    measure::{Latency, megabytes},
};
use std::time::Duration;

pub fn heading(title: &str) {
    println!("\n== {title}");
}

pub fn row(label: &str, value: String) {
    println!("  {label:<44}{value}");
}

pub fn latency_row(label: &str, measured: &Latency) {
    row(
        label,
        format!(
            "p50 {:>9}  mean {:>9}  max {:>9}  {} docs",
            millis(measured.median),
            millis(measured.mean),
            millis(measured.worst),
            measured.matches
        ),
    );
}

pub fn bytes(value: u64) -> String {
    format!("{value} B ({:.2} MiB)", megabytes(value))
}

pub fn millis(value: Duration) -> String {
    format!("{:.3} ms", value.as_secs_f64() * 1000.0)
}

/// Everything the whole-release estimate is scaled from.
pub struct Extrapolation<'a> {
    pub factor: f64,
    pub seed_bytes: u64,
    pub insert: Duration,
    pub indexes: &'a [(&'a str, Duration)],
    pub on_disk: &'a Footprint,
}

pub fn extrapolation(estimate: Extrapolation<'_>) {
    let Extrapolation {
        factor,
        seed_bytes,
        insert,
        indexes,
        on_disk,
    } = estimate;

    heading("extrapolation to the whole release (ESTIMATE, linear in document count)");
    row(
        "documents",
        format!("{:.0} ({factor:.0}x)", crate::WORLD_PLACES),
    );
    row("seed, gzip -9", bytes_estimate(seed_bytes, factor));
    row("insert time", millis_estimate(insert, factor));
    for (label, elapsed) in indexes {
        row(
            &format!("build index: {label}"),
            millis_estimate(*elapsed, factor),
        );
    }
    row(
        "data directory",
        format!(
            "~{:.0} MiB tables + {} journal",
            megabytes(on_disk.total - on_disk.journal) * factor,
            bytes(on_disk.journal)
        ),
    );
    println!(
        "\nIndex builds are super-linear (n log n), so those two are floors, and query latency on\n\
         an index grows with log(n) rather than n. Peak RSS is deliberately not extrapolated:\n\
         run this with 254 copies to measure it instead. Reads settle at the 256 MB WiredTiger\n\
         cache whatever the collection size, but a bulk load does not."
    );
}

fn bytes_estimate(value: u64, factor: f64) -> String {
    format!("~{:.0} MiB", megabytes(value) * factor)
}

fn millis_estimate(value: Duration, factor: f64) -> String {
    format!("~{:.1} s", value.as_secs_f64() * factor)
}
