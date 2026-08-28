//! A one-time pass that repairs directories an older build left with unmaintained indexes.
//!
//! Until the engine started calling `DatabaseHolder::openDb` for databases already on disk, a
//! collection loaded from a reopened directory carried an empty in-memory `IndexCatalog`.
//! Every write after that reopen went into the record store and into no index at all, `_id_`
//! included: a duplicate `_id` was accepted and both copies stayed, and documents written that
//! way are invisible to any query the planner answers from an index while a collection scan
//! still returns them. Counts disagree depending on the plan chosen, which is the worst shape
//! a data defect can take.
//!
//! The engine no longer does that, but opening such a directory with the fixed engine does not
//! undo it: the entries are still missing, and nothing says so until something asks.
//! `validate {repair: true}` is the engine's own answer -- it inserts the missing entries and
//! moves documents whose key turns out to be a duplicate into `local.lost_and_found.<uuid>`
//! rather than discarding them -- so this pass runs it, at most once per directory, over every
//! collection that needs it.
//!
//! It is the engine's general-purpose repair, not one written for this defect, and it has one
//! destructive branch: a record whose BSON cannot be read is deleted outright, with no lost and
//! found. Nothing this defect produces looks like that, but the trigger here is any validation
//! error, so `announce::announce_deletions` reports such a deletion on a line of its own.
//!
//! It lives in the Rust layer rather than in the C++ runtime on purpose. This is a migration
//! over user data, not engine semantics: a published engine build stays exactly as published,
//! and the pass can be read, tested and switched off without rebuilding it.
//!
//! ## What it costs
//!
//! `validate` is a full scan of a collection and its indexes, so the first open of a directory
//! that predates this pays for one over every collection in it. That is deliberate and not
//! hidden: it happens once, the marker it writes means no later open repeats it, and a
//! directory this build created is marked without being scanned at all. Set
//! [`SKIP_VARIABLE`] to opt out.
//!
//! ## What it does when something goes wrong
//!
//! Never fail the open. A directory that used to open must still open: the alternative to a
//! pass that could not run is exactly the state every already-published build is in, whereas
//! an open that refuses leaves the caller with no route to their data at all. Every failure is
//! reported through `tracing` at WARN and the client is returned regardless.
//!
//! A directory the process cannot write to never reaches this code at all. The engine's own
//! startup checkpoint fails to create `WiredTiger.turtle.set`, WiredTiger declares a panic and
//! MongoDB answers it with `fassert`, which aborts the host process --
//! `tests/durability/storage.rs` pins that. A marker that cannot be written is the survivable
//! version of the same thing, and costs only a repeat: the repair itself already ran, so the
//! failure is logged and the next open runs the pass again.
//!
//! A pass counts as complete when it has *visited* every collection, not when every collection
//! came out sound. Only a complete pass writes the marker, so a process that dies partway
//! through, or a `validate` that fails, is retried on the next open -- `validate {repair: true}`
//! is idempotent, and a collection already repaired validates clean and is left alone. A
//! collection that is still damaged after a repair does not hold the marker back: repeating a
//! repair that did not work on every open would trade a data defect for a startup cost without
//! fixing anything, so it is reported loudly and the pass moves on.

mod announce;
mod commands;
mod marker;
mod namespace;
mod origin;
mod report;

use crate::{Client, Error, Result};
use announce::{announce_damage, announce_repair, announce_residual_damage};
use commands::{Mode, namespaces, validate};
use marker::{Marker, MarkerState};
use namespace::Namespace;
pub(crate) use origin::{Origin, origin};
use report::{Damage, Health, Repaired};
use std::{env, ffi::OsStr, path::Path, time::Instant};

/// Set to `1`, `true`, `yes` or `on`, in any case, to leave the pass out entirely. Any other
/// value -- `no` and `off` included -- leaves it on; see [`SKIP_VALUES`].
///
/// Skipping never writes the marker, so it suppresses the pass rather than cancelling it: the
/// next open without the variable set still checks the directory.
pub(crate) const SKIP_VARIABLE: &str = "EMBEDDED_MONGODB_SKIP_INDEX_REPAIR";

/// Runs the pass if this directory has not had one, and records that it did.
pub(crate) fn run(client: &Client, data_directory: &Path, origin: Origin) {
    if skip_requested(env::var_os(SKIP_VARIABLE).as_deref()) {
        tracing::debug!(
            variable = SKIP_VARIABLE,
            "skipping the one-time index repair pass"
        );
        return;
    }

    let marker = Marker::in_data_directory(data_directory);
    if marker.state() == MarkerState::Recorded {
        return;
    }

    // Nothing this process is about to create can carry damage an older build did, so a new
    // directory is marked without a scan. That is what keeps the pass off the common path:
    // every directory made from here on is marked at birth and never pays for one.
    if origin == Origin::Fresh {
        record(&marker, data_directory);
        return;
    }

    let started = Instant::now();
    tracing::info!(
        directory = %data_directory.display(),
        "checking a directory written by an earlier build for missing index entries"
    );
    match pass(client) {
        Outcome::Complete => {
            tracing::info!(elapsed = ?started.elapsed(), "index repair pass finished");
            record(&marker, data_directory);
        }
        Outcome::Incomplete => tracing::warn!(
            elapsed = ?started.elapsed(),
            "the index repair pass could not check every collection; it will run again on the \
             next open"
        ),
    }
}

/// Whether a pass reached every collection it set out to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Outcome {
    Complete,
    Incomplete,
}

fn pass(client: &Client) -> Outcome {
    let catalog = match namespaces(client) {
        Ok(catalog) => catalog,
        Err(error) => {
            tracing::warn!(%error, "could not list the collections to check");
            return Outcome::Incomplete;
        }
    };

    // Databases that hold no collection at all is not an empty directory, it is what an engine
    // from before the fix looks like: `listCollections` reads the database holder, which such
    // an engine never populated for a database restored from disk. Linking this crate against a
    // library that old is not hypothetical -- the published manifest lags the fix -- and
    // recording the directory as checked would then skip its damage for good.
    if catalog.databases > 0 && catalog.namespaces.is_empty() {
        tracing::warn!(
            databases = catalog.databases,
            "the engine named databases but listed no collection in any of them, which is what \
             a library from before the DatabaseHolder::openDb fix does; leaving this directory \
             unmarked so a later open checks it again"
        );
        return Outcome::Incomplete;
    }

    sweep(&catalog.namespaces, |namespace| visit(client, namespace))
}

/// Runs `check` over every namespace and answers whether all of them were reached.
///
/// Separate from [`pass`] so the rule it encodes can be tested without an engine: one
/// collection that cannot be checked must not cost the rest their repair, and it must still
/// withhold the marker, so that the next open starts over rather than treating a half-finished
/// pass as a finished one.
fn sweep(namespaces: &[Namespace], mut check: impl FnMut(&Namespace) -> Result<()>) -> Outcome {
    let mut outcome = Outcome::Complete;
    for namespace in namespaces {
        if let Err(error) = check(namespace) {
            tracing::warn!(collection = %namespace, %error, "could not check this collection");
            outcome = Outcome::Incomplete;
        }
    }
    outcome
}

fn visit(client: &Client, namespace: &Namespace) -> Result<()> {
    let damage = match Health::parse(&validate(client, namespace, Mode::Check)?) {
        Health::Sound => return Ok(()),
        Health::Damaged(damage) => damage,
        // Not repaired and not passed over: a scan that threw says nothing about whether the
        // collection is sound, so the honest answer is that this one was never checked. The
        // error travels up to `sweep`, which withholds the marker and leaves the collection
        // for a later open rather than recording a verdict nobody reached.
        Health::Inconclusive(reason) => return Err(did_not_finish(namespace, &reason)),
    };
    announce_damage(namespace, &damage);

    let repaired = Repaired::parse(&validate(client, namespace, Mode::Repair)?);
    // Announced here, before anything else can fail, and never behind a `?`. By this point
    // documents may already have moved to another collection, and the record of where they
    // went must not be able to leave with an error raised by the confirmation below.
    announce_repair(namespace, &repaired);

    // The repair reply reports the state `validate` *found*, not the one it left: a collection
    // that is sound afterwards still comes back with `valid: false` in the same reply that
    // says `repaired: true`. Asking again is the only honest way to say whether it worked.
    match Health::parse(&validate(client, namespace, Mode::Check)?) {
        Health::Sound => Ok(()),
        Health::Damaged(residual) => {
            announce_residual_damage(namespace, &residual);
            Ok(())
        }
        // A confirmation that threw leaves the repair unconfirmed, which is not something to
        // record as done either.
        Health::Inconclusive(reason) => Err(did_not_finish(namespace, &reason)),
    }
}

fn did_not_finish(namespace: &Namespace, reason: &str) -> Error {
    Error::InvalidResponse(format!("validate on {namespace} did not finish: {reason}"))
}

fn record(marker: &Marker, data_directory: &Path) {
    // A marker that cannot be written is a cost, not a correctness problem: the repair itself
    // already ran, and the only consequence is that the next open runs it again.
    if let Err(error) = marker.record() {
        tracing::warn!(
            directory = %data_directory.display(),
            %error,
            "could not record that the index repair pass ran; it will run again on the next open"
        );
    }
}

/// The values that turn the pass off, and nothing else.
///
/// Deliberately a closed set, and deliberately not the "anything that is not empty, `0` or
/// `false`" rule `embedded-mongodb-sys/build.rs` uses for its build flags. This switch disables
/// a repair of damaged user data, so the two ways of misreading it are not equally bad:
/// declining to skip on a value nobody meant as yes costs a scan, while skipping on `no` or
/// `off` would leave a directory damaged for good, and silently.
const SKIP_VALUES: [&str; 4] = ["1", "true", "yes", "on"];

fn skip_requested(value: Option<&OsStr>) -> bool {
    let Some(value) = value.and_then(OsStr::to_str) else {
        return false;
    };
    let value = value.trim();
    SKIP_VALUES
        .iter()
        .any(|affirmative| value.eq_ignore_ascii_case(affirmative))
}

#[cfg(test)]
mod tests {
    use super::{Namespace, Outcome, SKIP_VARIABLE, skip_requested, sweep};
    use crate::Error;
    use std::{cell::RefCell, ffi::OsStr};

    #[test]
    fn the_skip_variable_is_off_when_it_is_not_set() {
        assert!(!skip_requested(None));
    }

    /// Anything that is not one of the four affirmatives leaves the pass on. `no` and `off`
    /// are the ones that matter: under a looser "not 0, not false" rule they would read as
    /// yes, and quietly turn off a repair of damaged data.
    #[test]
    fn the_skip_variable_is_off_for_every_value_that_is_not_an_affirmative() {
        for value in [
            "", "   ", "0", "false", "False", "FALSE", "no", "off", "disabled", "2", "maybe",
        ] {
            assert!(
                !skip_requested(Some(OsStr::new(value))),
                "{SKIP_VARIABLE}={value:?} should not skip the pass"
            );
        }
    }

    #[test]
    fn the_skip_variable_is_on_for_anything_that_reads_as_yes() {
        for value in [
            "1", "true", "TRUE", "yes", "Yes", "on", "ON", " 1 ", "\ttrue\n",
        ] {
            assert!(
                skip_requested(Some(OsStr::new(value))),
                "{SKIP_VARIABLE}={value:?} should skip the pass"
            );
        }
    }

    fn namespaces() -> Vec<Namespace> {
        vec![
            Namespace::new("shop", "orders"),
            Namespace::new("shop", "untouched"),
            Namespace::new("catalog", "items"),
        ]
    }

    #[test]
    fn a_sweep_that_reached_every_collection_is_complete() {
        let visited = RefCell::new(Vec::new());

        let outcome = sweep(&namespaces(), |namespace| {
            visited.borrow_mut().push(namespace.to_string());
            Ok(())
        });

        assert_eq!(outcome, Outcome::Complete);
        assert_eq!(
            visited.into_inner(),
            ["shop.orders", "shop.untouched", "catalog.items"]
        );
    }

    /// The rule the marker depends on: one collection that could not be checked leaves the
    /// pass incomplete, so the next open starts over instead of inheriting a half-finished one.
    #[test]
    fn one_collection_that_fails_leaves_the_sweep_incomplete() {
        let outcome = sweep(&namespaces(), |namespace| {
            if namespace.collection() == "untouched" {
                return Err(Error::InvalidArgument("no"));
            }
            Ok(())
        });

        assert_eq!(outcome, Outcome::Incomplete);
    }

    /// ...and it does not stop the collections after it from being checked, or a single
    /// unreadable collection would deny every later one its repair.
    #[test]
    fn a_failure_does_not_end_the_sweep() {
        let visited = RefCell::new(Vec::new());

        sweep(&namespaces(), |namespace| {
            visited.borrow_mut().push(namespace.to_string());
            Err(Error::InvalidArgument("no"))
        });

        assert_eq!(visited.into_inner().len(), 3);
    }

    #[test]
    fn a_sweep_over_no_collections_is_complete() {
        assert_eq!(sweep(&[], |_| Ok(())), Outcome::Complete);
    }
}
