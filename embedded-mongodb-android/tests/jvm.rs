//! Drives the JNI entry points from a real JVM, through the Java classes the Kotlin module
//! declares.
//!
//! The unit tests in `src/registry.rs` prove the handle and threading rules against a fake
//! client; this proves that the exported symbols, the exception class, the codes and the
//! `byte[]` round trip are what Java actually sees. Both harnesses run under `-Xcheck:jni`,
//! whose complaints fail the test -- chiefly a JNI call made while an exception is pending,
//! which is what a mishandled throw produces.
//!
//! `cargo test` builds this package's `rlib` but not its `cdylib`, so the harnesses load
//! `examples/jni_probe.rs`: a `cdylib` over that same `rlib`, exporting the same entry
//! points. Whenever a `cargo build` has left the real `libembedded_mongodb_android.so` beside
//! it, the bridge harness runs against that as well.
//!
//! A JDK is required. Every runner in this repository's CI has one, and so does any machine
//! that can build the Android library at all.

mod common;

use common::harness::run_harness;
use common::libraries::{bridge_libraries, probe_library};

#[test]
fn the_native_bridge_serves_a_real_jvm() {
    for library in bridge_libraries() {
        let output = run_harness("BridgeHarness", &library, "classes-bridge");
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

#[test]
fn a_panic_reaches_java_as_an_exception() {
    let output = run_harness("PanicHarness", &probe_library(), "classes-panic");
    assert!(
        output.contains("PASS a panic crosses as an exception")
            && output.contains("deliberate panic from the JNI boundary probe"),
        "the panic did not arrive as an exception:\n{output}"
    );
}
