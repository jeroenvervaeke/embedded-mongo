//! How the pass says what it found and what it did about it.
//!
//! Its own file because reporting is half of what this migration is for. Moving a user's
//! documents into another collection is only acceptable if the process says so, loudly and with
//! enough detail to find them again, so these three lines are as much the deliverable as the
//! repair itself.

use super::{Damage, Namespace, Repaired};

/// Said before the repair runs, so that a `validate {repair: true}` which dies partway through
/// still leaves the name of the collection it was working on behind.
pub(super) fn announce_damage(namespace: &Namespace, damage: &Damage) {
    tracing::warn!(
        collection = %namespace,
        missing_index_entries = damage.missing_index_entries,
        errors = ?damage.errors,
        "repairing a collection an earlier build left with missing index entries"
    );
}

/// Silent data movement is not acceptable, so everything that moved and where it went goes out
/// at WARN.
///
/// The message deliberately claims nothing about deletion. `validate {repair: true}` moves a
/// duplicate to the lost and found, but it *deletes* a record whose BSON does not validate --
/// `validate_adaptor.cpp` calls `deleteRecord` for those, with nowhere to recover them from --
/// and [`announce_deletions`] is what says so when it happens.
pub(super) fn announce_repair(namespace: &Namespace, repaired: &Repaired) {
    if !repaired.changed_anything() {
        return;
    }
    tracing::warn!(
        collection = %namespace,
        inserted_index_entries = repaired.inserted_index_entries,
        documents_moved = repaired.moved_documents,
        moved_to = repaired.lost_and_found.as_ref().map(tracing::field::display),
        removed_corrupt_records = repaired.removed_corrupt_records,
        removed_extra_index_entries = repaired.removed_extra_index_entries,
        engine_warnings = ?repaired.warnings,
        "repaired index entries an earlier build never wrote; documents evicted to keep a key \
         unique were moved to the collection named by moved_to"
    );
    announce_deletions(namespace, repaired);
    announce_missing_destination(namespace, repaired);
}

/// The one thing this pass can do that cannot be undone.
///
/// `validate {repair: true}` removes records whose BSON is malformed, and there is no lost and
/// found for those. It is a separate record rather than a field on the line above so that it
/// cannot be read past, and so that a log filtered to this message alone answers "did anything
/// get deleted".
fn announce_deletions(namespace: &Namespace, repaired: &Repaired) {
    if repaired.removed_corrupt_records == 0 {
        return;
    }
    tracing::warn!(
        collection = %namespace,
        removed_corrupt_records = repaired.removed_corrupt_records,
        "the repair DELETED records whose contents could not be read; unlike the duplicates it \
         moves, these are not recoverable from local.lost_and_found. Restore this collection \
         from a backup if those records mattered"
    );
}

/// Documents moved with nowhere named to look for them.
///
/// The destination is composed from the collection's UUID, so a reply without a readable one
/// leaves `moved_to` off the record above entirely -- `tracing` omits a `None` field rather
/// than printing it. That is the one case where the reader most needs an answer, so it gets one
/// of its own, pointing at the engine's own wording.
fn announce_missing_destination(namespace: &Namespace, repaired: &Repaired) {
    if repaired.moved_documents == 0 || repaired.lost_and_found.is_some() {
        return;
    }
    tracing::warn!(
        collection = %namespace,
        documents_moved = repaired.moved_documents,
        engine_warnings = ?repaired.warnings,
        "documents were moved out of this collection but the reply did not carry a readable \
         collection UUID, so this pass cannot name where; the engine's own warning in \
         engine_warnings does, and `listCollections` on `local` will show it"
    );
}

pub(super) fn announce_residual_damage(namespace: &Namespace, residual: &Damage) {
    tracing::warn!(
        collection = %namespace,
        missing_index_entries = residual.missing_index_entries,
        errors = ?residual.errors,
        "this collection is still damaged after a repair; run \
         `validate {{repair: true}}` against it by hand and check the result"
    );
}
