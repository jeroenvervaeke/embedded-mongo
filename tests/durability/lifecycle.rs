//! open -> insert -> close -> reopen, over and over. An Android app does this on every
//! foreground/background transition, so it has to cost nothing that accumulates.

use crate::probe::{Role, harness::Child, outcome::Outcome, scratch, workload::PER_CYCLE};

const REOPEN_CYCLES: i64 = 20;

/// How much the data directory is allowed to grow between the second cycle and the last. The
/// cycles add well under a megabyte of documents to a directory that starts out three orders of
/// magnitude larger, so anything the engine failed to reclaim on a close would dwarf this.
const ALLOWED_GROWTH: i64 = 1024 * 1024;

#[test]
fn data_survives_twenty_reopen_cycles() {
    let outcome = reopen_cycles();
    let readings = outcome.all("cycle");

    assert_eq!(readings.len(), REOPEN_CYCLES as usize);
    for (cycle, reading) in readings.iter().enumerate() {
        let cycle = cycle as i64;
        let expected = format!(
            "{cycle} before={} after={}",
            cycle * PER_CYCLE,
            (cycle + 1) * PER_CYCLE
        );
        assert!(
            reading.starts_with(&expected),
            "cycle {cycle} reported `{reading}`, expected it to start with `{expected}`\n{}",
            outcome.transcript()
        );
    }
}

#[test]
fn reopen_cycles_leak_no_space() {
    let outcome = reopen_cycles();
    let sizes = readings(&outcome, "bytes")
        .iter()
        .filter_map(|size| size.parse::<i64>().ok())
        .collect::<Vec<_>>();

    let (Some(settled), Some(last)) = (sizes.get(1), sizes.last()) else {
        panic!(
            "fewer than two cycles reported a size\n{}",
            outcome.transcript()
        );
    };
    assert!(
        last - settled < ALLOWED_GROWTH,
        "the directory grew {} bytes over {} cycles\n{}",
        last - settled,
        REOPEN_CYCLES - 1,
        outcome.transcript()
    );
}

#[test]
fn reopen_cycles_leak_no_descriptors() {
    let outcome = reopen_cycles();
    let descriptors = readings(&outcome, "descriptors");

    let Some(first) = descriptors.first() else {
        panic!(
            "no cycle reported a descriptor count\n{}",
            outcome.transcript()
        );
    };
    if first == "unavailable" {
        eprintln!("descriptor counts are read from /proc, which this platform does not have");
        return;
    }
    // The first cycle pays for descriptors the process opens lazily and keeps, so the
    // comparison starts at the second one.
    let steady = descriptors.get(1..).unwrap_or_default();
    assert!(
        steady.windows(2).all(|pair| pair.first() == pair.last()),
        "descriptor count moved across cycles: {descriptors:?}\n{}",
        outcome.transcript()
    );
}

fn reopen_cycles() -> Outcome {
    let directory = scratch::directory();
    let outcome = Child::spawn(
        Role::ReopenCycles,
        &directory.path().join("database"),
        REOPEN_CYCLES,
    )
    .finish();
    outcome.assert_exited_cleanly().report();
    outcome
}

/// Pulls one `key=value` field out of every `cycle` reading, in cycle order.
fn readings(outcome: &Outcome, key: &str) -> Vec<String> {
    let prefix = format!("{key}=");
    outcome
        .all("cycle")
        .iter()
        .filter_map(|reading| {
            reading
                .split_whitespace()
                .find_map(|field| field.strip_prefix(&prefix))
        })
        .map(str::to_owned)
        .collect()
}
