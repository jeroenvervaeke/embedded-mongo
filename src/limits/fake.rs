//! An engine the floors can be tested against without one being started.
//!
//! Shared by every test in this module rather than written twice: the floors are the process's,
//! so the same fake has to serve the tests of an open, of a move on a running client, and of the
//! knobs themselves. Every departure from a healthy engine is a [`Quirk`], one at a time,
//! because an engine that both hid a knob and refused it would be two tests in one.

use crate::{
    Error, IndexBuildFloor, QuerySpillingFloor, Result,
    limits::{
        AdminCommands, FreeDiskFloor, ReportedFloors,
        knobs::{INDEX_BUILD_FLOOR, QUERY_SPILLING_FLOOR},
        rendezvous::Rendezvous,
    },
};
use bson::{Document, doc};
use std::sync::{Mutex, PoisonError};

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
/// against it could tell those two apart.
///
/// Behind mutexes rather than `RefCell`s so that two threads can share one, which is what the
/// serialisation of a floor move has to be proved against.
pub(crate) struct FakeEngine {
    floors: Mutex<ReportedFloors>,
    commands: Mutex<Vec<Document>>,
    quirk: Quirk,
}

impl FakeEngine {
    /// What a mistyped knob is answered with, so that a test can insist the failure names it.
    pub(crate) const MISTYPED_AS: &str = "not a number at all";

    pub(crate) fn new(index_build_mebibytes: i64, query_spilling_bytes: i64) -> Self {
        Self {
            floors: Mutex::new(ReportedFloors::new(
                IndexBuildFloor::from_mebibytes(index_build_mebibytes),
                QuerySpillingFloor::from_bytes(query_spilling_bytes),
            )),
            commands: Mutex::new(Vec::new()),
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

    /// An engine that takes the parameters before the `nth` and refuses every one from it on,
    /// counting from zero -- so a move can be failed and the retreat that follows it failed too.
    pub(crate) fn refusing_from(mut self, nth: usize) -> Self {
        self.quirk = Quirk::RefusesFrom(nth);
        self
    }

    pub(crate) fn missing(mut self, knob: &'static str) -> Self {
        self.quirk = Quirk::Hides(knob);
        self
    }

    pub(crate) fn mistyping(mut self, knob: &'static str) -> Self {
        self.quirk = Quirk::Mistypes(knob);
        self
    }

    pub(crate) fn pausing_in_the_index_build_knob(mut self) -> Self {
        self.quirk = Quirk::Pauses(Rendezvous::new());
        self
    }

    pub(crate) fn reported(&self) -> ReportedFloors {
        *self.floors.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn floors_set(&self) -> Vec<Document> {
        self.sent()
            .iter()
            .filter(|command| command.contains_key("setParameter"))
            .cloned()
            .collect()
    }

    fn sent(&self) -> std::sync::MutexGuard<'_, Vec<Document>> {
        self.commands.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn read(&self) -> Document {
        assert!(
            !matches!(self.quirk, Quirk::NeverRead),
            "the floors were read here rather than before the first floor moved"
        );
        let floors = self.reported();
        let mut reply = doc! { "ok": 1.0 };
        for (knob, value) in [
            (INDEX_BUILD_FLOOR, floors.index_build().mebibytes()),
            (QUERY_SPILLING_FLOOR, floors.query_spilling().bytes()),
        ] {
            match &self.quirk {
                Quirk::Hides(hidden) if *hidden == knob => {}
                Quirk::Mistypes(mistyped) if *mistyped == knob => {
                    reply.insert(knob, Self::MISTYPED_AS);
                }
                _ => {
                    reply.insert(knob, value);
                }
            }
        }
        reply
    }

    /// The knob this engine will not take, if this command carries it.
    fn refused(&self, command: &Document) -> Option<&'static str> {
        let carried = knob_of(command)?;
        match self.quirk {
            Quirk::Refuses(knob) if knob == carried => Some(carried),
            // Counting the one being answered, which is already recorded.
            Quirk::RefusesFrom(nth) if self.floors_set().len() > nth => Some(carried),
            _ => None,
        }
    }

    /// Holds the thread inside the index-build knob until a second mover reaches it, so that a
    /// pair of moves which is not serialised interleaves here rather than by luck.
    fn pause(&self, command: &Document) {
        if let Quirk::Pauses(rendezvous) = &self.quirk
            && command.contains_key(INDEX_BUILD_FLOOR)
        {
            rendezvous.wait_for_another();
        }
    }
}

impl AdminCommands for FakeEngine {
    fn run_on_admin(&self, command: &Document) -> Result<Document> {
        self.sent().push(command.clone());
        if command.contains_key("getParameter") {
            return Ok(self.read());
        }
        self.pause(command);
        if let Some(knob) = self.refused(command) {
            return Err(Error::Server {
                code: Some(72),
                message: format!("no such parameter {knob}"),
                response: Box::new(doc! { "ok": 0.0 }),
            });
        }
        let mut floors = self.floors.lock().unwrap_or_else(PoisonError::into_inner);
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
enum Quirk {
    None,
    /// A knob this engine does not have, refused the way a renamed one would be.
    Refuses(&'static str),
    /// An engine that stops taking parameters from the nth one on, so that a move and the
    /// retreat that follows it can both be refused.
    RefusesFrom(usize),
    /// A knob this engine will not report, left out of every `getParameter` reply.
    Hides(&'static str),
    /// A knob this engine reports as something other than the integer it holds.
    Mistypes(&'static str),
    /// Floors that cannot be read at all, so a test can prove no read was taken.
    NeverRead,
    /// An index-build knob slow enough that a second mover has time to reach it.
    Pauses(Rendezvous),
}

/// Which of the two floors a `setParameter` command carries.
fn knob_of(command: &Document) -> Option<&'static str> {
    [INDEX_BUILD_FLOOR, QUERY_SPILLING_FLOOR]
        .into_iter()
        .find(|knob| command.contains_key(knob))
}
