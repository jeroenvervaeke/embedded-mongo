//! Drives the JNI entry points from a real JVM, through the Java classes the Kotlin module
//! declares.
//!
//! The unit tests in `src/registry.rs` prove the handle and threading rules against a fake
//! client; this proves that the exported symbols, the exception class, the codes, the `byte[]`
//! round trip and the one-time index repair pass are what Java actually sees. Every harness
//! runs under `-Xcheck:jni`, whose complaints fail the test -- chiefly a JNI call made while an
//! exception is pending, which is what a mishandled throw produces.
//!
//! `cargo test` builds this package's `rlib` but not its `cdylib`, so the harnesses load
//! `examples/jni_probe.rs`: a `cdylib` over that same `rlib`, exporting the same entry
//! points. Whenever a `cargo build` has left the real `libembedded_mongodb_android.so` beside
//! it, the bridge and repair harnesses run against that as well.
//!
//! A JDK is required. Every runner in this repository's CI has one, and so does any machine
//! that can build the Android library at all.

mod common;

/// The same helper the root crate's repair tests use, compiled into this target rather than
/// copied: one unpacking of the fixture, and one place its file count is asserted.
#[path = "../../tests/repair/fixture.rs"]
mod fixture;

use common::harness::{Database, run_harness};
use common::libraries::{bridge_libraries, probe_library};

#[test]
fn the_native_bridge_serves_a_real_jvm() {
    for library in bridge_libraries() {
        let output = run_harness("BridgeHarness", &library, "classes-bridge", Database::Own);
        for expected in [
            "PASS open rejects an unusable path",
            "PASS a MongoDB error code survives the boundary: code=13180000",
            "PASS ping answers",
            "PASS a refused command answers ok: 0 with code",
            "PASS null arguments are rejected",
            "PASS forged, zero and out-of-range handles throw",
            "PASS 400 commands ran across 8 threads",
            "PASS close under load left every command cleanly refused",
            "PASS close is final",
            "PASS all",
        ] {
            assert!(
                output.contains(expected),
                "missing `{expected}` from {}:\n{output}",
                library.display()
            );
        }
        assert!(
            output.contains("byte command returned"),
            "the megabyte round trip did not run:\n{output}"
        );
    }
}

/// The bridge opens through `embedded_mongodb::Client`, so a directory an older build damaged
/// is repaired on the way in. It used to open the raw FFI client, which skips the pass, and an
/// Android application pointed at a directory some earlier build wrote is the likeliest holder
/// of that damage.
///
/// A fresh copy of the fixture per library: the pass marks the directory it repaired, so a
/// second run over the same one would find nothing left to do and prove nothing.
#[test]
fn the_bridge_repairs_a_directory_an_older_build_damaged() {
    let scratch = fixture::directory();
    for (index, library) in bridge_libraries().into_iter().enumerate() {
        let damaged = scratch.path().join(format!("damaged-{index}"));
        fixture::unpack_damaged(&damaged);

        let output = run_harness(
            "RepairHarness",
            &library,
            "classes-repair",
            Database::Existing(&damaged),
        );

        for expected in [
            "PASS opening through the bridge ran the index repair pass",
            "PASS the documents the damaged indexes hid are indexed again",
            // The collection it names is composed from the collection's UUID, so matching the
            // prefix is matching a name the engine really created.
            "PASS the duplicate _id was moved to local.lost_and_found.",
            "PASS the _id index refuses a duplicate again",
            "PASS all",
        ] {
            assert!(
                output.contains(expected),
                "missing `{expected}` from {}:\n{output}",
                library.display()
            );
        }
        assert!(
            fixture::marker_exists(&damaged),
            "{} left the directory unmarked, so every later open would scan it again",
            library.display()
        );
    }
}

/// The storage limits, both mechanisms, against a real engine.
///
/// The unit tests in `src/options.rs` prove the slot vector becomes the right `OpenOptions`;
/// this proves the entry point is bound, that what it was handed reached WiredTiger, and that
/// the free-disk floor moves over the `command` entry point alone -- which is what lets the
/// Kotlin side set it without a native symbol of its own.
#[test]
fn the_storage_limits_reach_the_engine_through_the_bridge() {
    for library in bridge_libraries() {
        let output = run_harness("OptionsHarness", &library, "classes-options", Database::Own);
        for expected in [
            "PASS a null option vector is rejected",
            "PASS limits outside the engine's range are refused before it opens",
            "PASS openWithOptions reads what it knows and ignores the rest",
            "PASS the cache and journal limits reached WiredTiger",
            "PASS journal pre-allocation reached WiredTiger",
            "PASS the engine starts on MongoDB's 500 MB floor",
            "PASS the floor moved without a native entry point of its own",
            "PASS an index build is refused below the floor",
            "PASS lowering the floor is what lets the index build run",
            "PASS all",
        ] {
            assert!(
                output.contains(expected),
                "missing `{expected}` from {}:\n{output}",
                library.display()
            );
        }
    }
}

/// A caller that fills in fewer slots than this build reads -- the shape every growth of the
/// option vector has to keep working, and the shape the Kotlin side sends whenever a caller
/// names some limits and not others.
#[test]
fn a_short_option_vector_defaults_the_slots_it_leaves_off() {
    for library in bridge_libraries() {
        let output = run_harness(
            "PartialOptionsHarness",
            &library,
            "classes-partial-options",
            Database::Own,
        );
        for expected in [
            "PASS a short option vector names one limit and defaults the rest",
            "PASS all",
        ] {
            assert!(
                output.contains(expected),
                "missing `{expected}` from {}:\n{output}",
                library.display()
            );
        }
    }
}

#[test]
fn a_panic_reaches_java_as_an_exception() {
    let output = run_harness(
        "PanicHarness",
        &probe_library(),
        "classes-panic",
        Database::Own,
    );
    assert!(
        output.contains("PASS a panic crosses as an exception")
            && output.contains("deliberate panic from the JNI boundary probe"),
        "the panic did not arrive as an exception:\n{output}"
    );
}
