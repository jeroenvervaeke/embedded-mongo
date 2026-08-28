//! Reads back what the repair pass reported, because reporting is half of what it is for.
//!
//! Moving a user's documents into another collection is only acceptable if the process says so,
//! so the log line is a behaviour under test rather than decoration. `tracing` records go
//! nowhere without a subscriber, so these tests install one over a buffer they can drain.

use std::{
    io::{self, Write},
    sync::{Arc, Mutex},
};
use tracing::{Level, Metadata};
use tracing_subscriber::fmt::MakeWriter;

/// The pass's own events. The engine forwards its log through `tracing` too, at INFO and in
/// volume, and none of it is what these tests are reading.
const TARGET: &str = "embedded_mongodb::repair";

/// Everything the crate has logged since the last [`Recorder::take`].
#[derive(Clone, Default)]
pub struct Recorder {
    written: Arc<Mutex<Vec<u8>>>,
}

impl Recorder {
    /// Installs this recorder as the process-wide subscriber. Once per process: `tracing`
    /// refuses a second global default, which is why the tests here share one.
    pub fn install() -> Self {
        let recorder = Self::default();
        tracing_subscriber::fmt()
            .with_max_level(Level::INFO)
            .with_writer(recorder.clone())
            .without_time()
            .with_ansi(false)
            .init();
        recorder
    }

    /// Everything logged since the last call, leaving the buffer empty.
    pub fn take(&self) -> String {
        let mut written = self
            .written
            .lock()
            .expect("the recorder mutex is never poisoned");
        String::from_utf8_lossy(&std::mem::take(&mut *written)).into_owned()
    }
}

/// Where one event goes: into the buffer, or nowhere.
pub enum Sink<'a> {
    Record(&'a Recorder),
    Discard,
}

impl Write for Sink<'_> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if let Self::Record(recorder) = self {
            let mut written = recorder
                .written
                .lock()
                .expect("the recorder mutex is never poisoned");
            written.extend_from_slice(buffer);
        }
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for Recorder {
    type Writer = Sink<'a>;

    fn make_writer(&'a self) -> Self::Writer {
        Sink::Record(self)
    }

    fn make_writer_for(&'a self, metadata: &Metadata<'_>) -> Self::Writer {
        if metadata.target().starts_with(TARGET) {
            Sink::Record(self)
        } else {
            Sink::Discard
        }
    }
}

/// The line the pass writes when it starts checking a directory it has not seen before. Its
/// absence is how a test asserts the pass did not run at all, which no other observation can
/// distinguish from a pass that ran and found nothing.
pub const PASS_RAN: &str = "checking a directory written by an earlier build";

/// The warning that says documents were moved.
pub const REPAIRED: &str = "repaired index entries an earlier build never wrote";

/// Said before a repair starts. Its absence is a sharper "nothing was repaired" than
/// [`REPAIRED`], which is gated on the repair having changed something and so stays quiet for a
/// repair that ran and did nothing.
pub const REPAIRING: &str = "repairing a collection an earlier build left with";

/// The warning for a collection a repair could not fix.
pub const STILL_DAMAGED: &str = "still damaged after a repair";

/// The warning for records `validate {repair: true}` removed outright, which unlike an evicted
/// duplicate are not recoverable.
pub const DELETED: &str = "the repair DELETED records";

/// The value of a `key=value` field in the captured log, so an assertion can compare it with
/// something read back from the engine instead of only matching a prefix.
pub fn field(haystack: &str, key: &str) -> String {
    let Some((_, rest)) = haystack.split_once(&format!("{key}=")) else {
        panic!("no `{key}=` field in the log:\n{haystack}");
    };
    rest.split_whitespace()
        .next()
        .unwrap_or_default()
        .to_owned()
}

pub fn assert_contains(haystack: &str, needle: &str) {
    assert!(
        haystack.contains(needle),
        "expected the log to contain `{needle}`, got:\n{haystack}"
    );
}

pub fn assert_absent(haystack: &str, needle: &str) {
    assert!(
        !haystack.contains(needle),
        "expected the log not to contain `{needle}`, got:\n{haystack}"
    );
}
