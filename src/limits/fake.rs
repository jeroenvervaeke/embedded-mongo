//! An engine the floors can be tested against without one being started.
//!
//! Shared by every test in this module rather than written twice: the floors are the process's,
//! so the same fake has to serve the tests of an open and of a move on a running client. Every
//! departure from a healthy engine is a [`Quirk`], one at a time, because an engine that both
//! hid a knob and refused it would be two tests in one.

use crate::{
    Error, IndexBuildFloor, QuerySpillingFloor, Result,
    limits::{
        AdminCommands, FreeDiskFloor, ReportedFloors,
        knobs::{INDEX_BUILD_FLOOR, QUERY_SPILLING_FLOOR},
    },
};
use bson::{Document, doc};
use std::cell::RefCell;

/// MongoDB's own floors, and the ones a fresh fake engine starts on.
pub(crate) const DEFAULT_MEBIBYTES: i64 = 500;
pub(crate) const DEFAULT_BYTES: i64 = DEFAULT_MEBIBYTES * 1024 * 1024;

pub(crate) fn floor(mebibytes: u32) -> FreeDiskFloor {
    FreeDiskFloor::from_mebibytes(mebibytes).expect("a floor inside the accepted range")
}

/// The two `setParameter` commands carrying these floors, spelled out here rather than built by
/// the code under test -- an expectation the production helper produced would agree with itself.
pub(crate) fn floors_set(mebibytes: i64, bytes: i64) -> Vec<Document> {
    vec![
        doc! { "setParameter": 1, "indexBuildMinAvailableDiskSpaceMB": mebibytes },
        doc! {
            "setParameter": 1,
            "internalQuerySpillingMinAvailableDiskSpaceBytes": bytes,
        },
    ]
}

/// An engine that answers `getParameter` with whatever `setParameter` last wrote.
///
/// Remembering rather than fixed because these tests turn on *when* the floors are read: an open
/// has to record MongoDB's own before applying the caller's, or it records the caller's and
/// hands it to the next open that asked for the default. A fake whose reply ignores what was set
/// on it answers a read taken after a write exactly as one taken before, so no test written
/// against it could tell those two apart. A move that has to put a floor back turns on the same
/// thing from the other end: what it puts back is what it read.
pub(crate) struct FakeEngine {
    floors: RefCell<ReportedFloors>,
    commands: RefCell<Vec<Document>>,
    quirk: Quirk,
}

impl FakeEngine {
    pub(crate) fn new(index_build_mebibytes: i64, query_spilling_bytes: i64) -> Self {
        Self {
            floors: RefCell::new(ReportedFloors::new(
                IndexBuildFloor::from_mebibytes(index_build_mebibytes),
                QuerySpillingFloor::from_bytes(query_spilling_bytes),
            )),
            commands: RefCell::new(Vec::new()),
            quirk: Quirk::None,
        }
    }

    pub(crate) fn reporting(mebibytes: i64) -> Self {
        Self::new(mebibytes, mebibytes * 1024 * 1024)
    }

    /// An engine that fails the test if its floors are read, for proving that a later open
    /// answers from what was recorded at the first one.
    pub(crate) fn never_read() -> Self {
        Self {
            quirk: Quirk::NeverRead,
            ..Self::reporting(DEFAULT_MEBIBYTES)
        }
    }

    pub(crate) fn refusing(mut self, knob: &'static str) -> Self {
        self.quirk = Quirk::Refuses(knob);
        self
    }

    pub(crate) fn missing(mut self, knob: &'static str) -> Self {
        self.quirk = Quirk::Hides(knob);
        self
    }

    pub(crate) fn reported(&self) -> ReportedFloors {
        *self.floors.borrow()
    }

    pub(crate) fn floors_set(&self) -> Vec<Document> {
        self.commands
            .borrow()
            .iter()
            .filter(|command| command.contains_key("setParameter"))
            .cloned()
            .collect()
    }

    fn read(&self) -> Document {
        assert!(
            self.quirk != Quirk::NeverRead,
            "the floors were read here rather than before the first floor moved"
        );
        let floors = self.reported();
        let mut reply = doc! { "ok": 1.0 };
        for (knob, value) in [
            (INDEX_BUILD_FLOOR, floors.index_build().mebibytes()),
            (QUERY_SPILLING_FLOOR, floors.query_spilling().bytes()),
        ] {
            if self.quirk != Quirk::Hides(knob) {
                reply.insert(knob, value);
            }
        }
        reply
    }
}

impl AdminCommands for FakeEngine {
    fn run_on_admin(&self, command: &Document) -> Result<Document> {
        self.commands.borrow_mut().push(command.clone());
        if command.contains_key("getParameter") {
            return Ok(self.read());
        }
        if let Quirk::Refuses(knob) = self.quirk
            && command.contains_key(knob)
        {
            return Err(Error::Server {
                code: Some(72),
                message: format!("no such parameter {knob}"),
                response: Box::new(doc! { "ok": 0.0 }),
            });
        }
        let mut floors = self.floors.borrow_mut();
        *floors = ReportedFloors::new(
            command
                .get_i64(INDEX_BUILD_FLOOR)
                .map_or(floors.index_build(), IndexBuildFloor::from_mebibytes),
            command
                .get_i64(QUERY_SPILLING_FLOOR)
                .map_or(floors.query_spilling(), QuerySpillingFloor::from_bytes),
        );
        Ok(doc! { "ok": 1.0 })
    }
}

/// What one fake engine does that a healthy one would not.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Quirk {
    None,
    /// A knob this engine does not have, refused the way a renamed one would be.
    Refuses(&'static str),
    /// A knob this engine will not report, left out of every `getParameter` reply.
    Hides(&'static str),
    /// Floors that cannot be read at all, so a test can prove no read was taken.
    NeverRead,
}
