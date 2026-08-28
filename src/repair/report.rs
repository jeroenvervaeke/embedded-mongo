//! Everything the pass believes about a collection, parsed out of a `validate` reply once.
//!
//! The boundary is here on purpose: below this file nothing reaches into a `Document` to ask
//! whether a collection is damaged or what a repair did, so a field MongoDB renames breaks one
//! parser rather than five call sites.

use super::Namespace;
use bson::{Bson, Document};

/// What a plain `validate` said about one collection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Health {
    Sound,
    Damaged(Damage),
    /// The scan did not finish, and said so only in passing.
    ///
    /// `validate` catches every exception but an interrupt, records it as a *warning* and still
    /// answers `ok: 1` — see the `catch (const DBException&)` at the end of `validate` in
    /// `mongo/src/mongo/db/validate/collection_validation.cpp`. `valid` is computed from the
    /// errors, which are empty, so a collection whose scan threw is indistinguishable from a
    /// sound one except by that one warning. Reading it as sound would record the directory as
    /// checked and never look at it again, which is the single outcome this pass must not
    /// produce.
    Inconclusive(String),
}

/// Why a collection needs repairing, in the terms `validate` reported it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Damage {
    pub(crate) errors: Vec<String>,
    /// A floor, not a total: the reply's `missingIndexEntries` array is capped, and a
    /// collection past the cap says so in an error rather than listing the rest.
    pub(crate) missing_index_entries: usize,
}

/// What `validate {repair: true}` did to one collection.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Repaired {
    pub(crate) inserted_index_entries: i64,
    pub(crate) moved_documents: i64,
    pub(crate) removed_corrupt_records: i64,
    pub(crate) removed_extra_index_entries: i64,
    /// Where the evicted duplicates went, present only when there were any.
    pub(crate) lost_and_found: Option<Namespace>,
    /// The engine's own prose, carried through untouched so the log says what it said rather
    /// than only this crate's summary of it.
    pub(crate) warnings: Vec<String>,
}

/// How the engine words a validation that threw. Prose, because the reply carries the fact
/// nowhere else -- no field, no `ok: 0`, not even `valid: false`. Matching it is the only way
/// to tell that answer apart from a clean one; if MongoDB rewords it this reads as sound again,
/// which is where the pass stood before the check existed rather than anywhere worse.
const VALIDATION_THREW: &str = "exception during collection validation";

impl Health {
    /// A missing or unreadable `valid` field counts as damage. The reply is then not the one
    /// this parser was written against, and of the two ways to be wrong -- repairing a sound
    /// collection, which costs a scan and changes nothing, or passing over a damaged one,
    /// which leaves it damaged for good -- only the first is recoverable.
    pub(crate) fn parse(reply: &Document) -> Self {
        // Before anything else: a scan that did not finish has no opinion worth having, and
        // its `valid: true` is an artefact of there being no errors rather than a verdict.
        if let Some(reason) = strings(reply, "warnings")
            .into_iter()
            .find(|warning| warning.contains(VALIDATION_THREW))
        {
            return Self::Inconclusive(reason);
        }

        let errors = strings(reply, "errors");
        let missing_index_entries = reply.get_array("missingIndexEntries").map_or(0, Vec::len);
        if reply.get_bool("valid").unwrap_or(false)
            && errors.is_empty()
            && missing_index_entries == 0
        {
            return Self::Sound;
        }
        Self::Damaged(Damage {
            errors,
            missing_index_entries,
        })
    }
}

impl Repaired {
    pub(crate) fn parse(reply: &Document) -> Self {
        let moved_documents = integer(reply, "numDocumentsMovedToLostAndFound");
        Self {
            inserted_index_entries: integer(reply, "numInsertedMissingIndexEntries"),
            moved_documents,
            removed_corrupt_records: integer(reply, "numRemovedCorruptRecords"),
            removed_extra_index_entries: integer(reply, "numRemovedExtraIndexEntries"),
            lost_and_found: lost_and_found(reply, moved_documents),
            warnings: strings(reply, "warnings"),
        }
    }

    /// Whether the repair touched anything, which is what decides between a warning naming
    /// what moved and silence. Computed rather than stored: it is a question about the four
    /// counters above and cannot be allowed to disagree with them.
    pub(crate) fn changed_anything(&self) -> bool {
        self.inserted_index_entries > 0
            || self.moved_documents > 0
            || self.removed_corrupt_records > 0
            || self.removed_extra_index_entries > 0
    }
}

/// The namespace the engine moves evicted duplicates into.
///
/// Composed here from the collection's UUID rather than read out of the reply, because the
/// reply carries it only inside a prose warning -- `index_repair.cpp` builds the name as
/// `local.lost_and_found.<uuid>` and puts it in a sentence. The UUID itself is a typed field,
/// so this is the one route to the name that does not depend on the wording.
fn lost_and_found(reply: &Document, moved_documents: i64) -> Option<Namespace> {
    if moved_documents == 0 {
        return None;
    }
    let Some(Bson::Binary(binary)) = reply.get("uuid") else {
        return None;
    };
    let uuid = binary.to_uuid().ok()?;
    Some(Namespace::new("local", format!("lost_and_found.{uuid}")))
}

fn strings(document: &Document, key: &str) -> Vec<String> {
    document.get_array(key).map_or_else(
        |_| Vec::new(),
        |values| {
            values
                .iter()
                .filter_map(Bson::as_str)
                .map(ToOwned::to_owned)
                .collect()
        },
    )
}

fn integer(document: &Document, key: &str) -> i64 {
    match document.get(key) {
        Some(Bson::Int32(value)) => i64::from(*value),
        Some(Bson::Int64(value)) => *value,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::{Damage, Health, Namespace, Repaired};
    use bson::doc;

    /// The reply the engine gives for a collection nothing is wrong with, trimmed to the
    /// fields this parser reads.
    fn sound() -> bson::Document {
        doc! {
            "valid": true,
            "repaired": false,
            "warnings": [],
            "errors": [],
            "extraIndexEntries": [],
            "missingIndexEntries": [],
            "nrecords": 4_i32,
            "nIndexes": 1_i32,
            "ok": 1.0,
        }
    }

    #[test]
    fn a_clean_validate_reply_is_sound() {
        assert_eq!(Health::parse(&sound()), Health::Sound);
    }

    #[test]
    fn errors_alone_are_damage() {
        let mut reply = sound();
        reply.insert("valid", false);
        reply.insert("errors", vec!["Found errors in _id_"]);

        assert_eq!(
            Health::parse(&reply),
            Health::Damaged(Damage {
                errors: vec!["Found errors in _id_".to_owned()],
                missing_index_entries: 0,
            })
        );
    }

    /// The shape the defect actually produces: entries missing from `_id_` and from a
    /// secondary index, reported one document per entry.
    #[test]
    fn missing_index_entries_are_counted() {
        let mut reply = sound();
        reply.insert("valid", false);
        reply.insert("errors", vec!["Found errors in _id_"]);
        reply.insert(
            "missingIndexEntries",
            vec![
                doc! { "indexName": "_id_", "recordId": 5_i64 },
                doc! { "indexName": "customer_1", "recordId": 5_i64 },
            ],
        );

        let Health::Damaged(damage) = Health::parse(&reply) else {
            panic!("a reply with missing index entries is damage");
        };
        assert_eq!(damage.missing_index_entries, 2);
    }

    /// `valid: true` with entries listed cannot happen in practice, but the parser must not be
    /// the thing that decides it cannot: the entries are the damage this pass exists for.
    #[test]
    fn missing_index_entries_outweigh_a_valid_flag() {
        let mut reply = sound();
        reply.insert(
            "missingIndexEntries",
            vec![doc! { "indexName": "_id_", "recordId": 5_i64 }],
        );

        assert!(matches!(Health::parse(&reply), Health::Damaged(_)));
    }

    /// The shape that would otherwise be read as a clean bill of health: `validate` caught an
    /// exception, recorded it as a warning, and answered `ok: 1` with `valid: true` and no
    /// errors. Reading that as sound would mark the directory checked over a collection whose
    /// scan never finished.
    #[test]
    fn a_validation_that_threw_is_inconclusive_rather_than_sound() {
        let mut reply = sound();
        reply.insert(
            "warnings",
            vec!["exception during collection validation: WriteConflict"],
        );

        assert_eq!(
            Health::parse(&reply),
            Health::Inconclusive(
                "exception during collection validation: WriteConflict".to_owned()
            )
        );
    }

    /// ...and it outranks the damage verdict too, because a scan that threw cannot be trusted
    /// to have found everything, so repairing on what it did report and then recording the
    /// collection as done would be worse than leaving it for the next open.
    #[test]
    fn a_validation_that_threw_outranks_the_errors_it_managed_to_record() {
        let mut reply = sound();
        reply.insert("valid", false);
        reply.insert("errors", vec!["Found errors in _id_"]);
        reply.insert(
            "warnings",
            vec!["exception during collection validation: Interrupted"],
        );

        assert!(matches!(Health::parse(&reply), Health::Inconclusive(_)));
    }

    /// The warnings a healthy check and a successful repair produce must not be mistaken for
    /// one, or every repaired collection would keep the marker from ever being written.
    #[test]
    fn ordinary_warnings_do_not_make_a_reply_inconclusive() {
        let mut reply = sound();
        reply.insert(
            "warnings",
            vec![
                "Inserted 5 missing index entries.",
                "Updated index multikey metadata",
            ],
        );

        assert_eq!(Health::parse(&reply), Health::Sound);
    }

    #[test]
    fn a_reply_without_a_valid_field_is_treated_as_damaged() {
        let mut reply = sound();
        reply.remove("valid");

        assert!(matches!(Health::parse(&reply), Health::Damaged(_)));
    }

    #[test]
    fn a_repair_reply_reports_what_it_inserted_and_moved() {
        let uuid = bson::Uuid::parse_str("a3547e5b-3fd3-4e75-988e-fc4aa2a15cb3").unwrap();
        let reply = doc! {
            "valid": false,
            "repaired": true,
            "uuid": uuid,
            "warnings": [
                "Inserted 5 missing index entries.",
                "Removed 1 duplicate documents to resolve 1 missing index entries.",
            ],
            "errors": [],
            "numRemovedCorruptRecords": 0_i32,
            "numRemovedExtraIndexEntries": 0_i32,
            "numInsertedMissingIndexEntries": 5_i32,
            "numDocumentsMovedToLostAndFound": 1_i32,
            "numOutdatedMissingIndexEntry": 0_i32,
            "ok": 1.0,
        };

        let repaired = Repaired::parse(&reply);

        assert_eq!(repaired.inserted_index_entries, 5);
        assert_eq!(repaired.moved_documents, 1);
        assert_eq!(
            repaired.lost_and_found,
            Some(Namespace::new(
                "local",
                "lost_and_found.a3547e5b-3fd3-4e75-988e-fc4aa2a15cb3"
            ))
        );
        assert_eq!(repaired.warnings.len(), 2);
        assert!(repaired.changed_anything());
    }

    /// A repair that moved nothing must not name a destination, or the log would send someone
    /// looking for a collection that was never created.
    #[test]
    fn a_repair_that_moved_nothing_names_no_lost_and_found() {
        let uuid = bson::Uuid::parse_str("a3547e5b-3fd3-4e75-988e-fc4aa2a15cb3").unwrap();
        let reply = doc! {
            "valid": true,
            "repaired": true,
            "uuid": uuid,
            "warnings": [ "Inserted 1 missing index entries." ],
            "numInsertedMissingIndexEntries": 1_i32,
            "numDocumentsMovedToLostAndFound": 0_i32,
            "ok": 1.0,
        };

        let repaired = Repaired::parse(&reply);

        assert_eq!(repaired.lost_and_found, None);
        assert!(repaired.changed_anything());
    }

    #[test]
    fn a_repair_that_changed_nothing_says_so() {
        let repaired = Repaired::parse(&doc! { "valid": true, "repaired": false, "ok": 1.0 });

        assert!(!repaired.changed_anything());
        assert_eq!(repaired, Repaired::default());
    }

    /// The counters come back as `Int32` today; a server that widened them must not silently
    /// read as zero and turn a repair into a silent one.
    #[test]
    fn counters_are_read_whether_they_arrive_as_int32_or_int64() {
        let reply = doc! {
            "numInsertedMissingIndexEntries": 7_i64,
            "numDocumentsMovedToLostAndFound": 2_i64,
            "numRemovedCorruptRecords": 3_i64,
            "numRemovedExtraIndexEntries": 4_i32,
        };

        let repaired = Repaired::parse(&reply);

        assert_eq!(repaired.inserted_index_entries, 7);
        assert_eq!(repaired.moved_documents, 2);
        assert_eq!(repaired.removed_corrupt_records, 3);
        assert_eq!(repaired.removed_extra_index_entries, 4);
    }

    #[test]
    fn corrupt_records_alone_count_as_a_change() {
        let repaired = Repaired::parse(&doc! { "numRemovedCorruptRecords": 1_i32 });

        assert!(repaired.changed_anything());
    }

    #[test]
    fn extra_index_entries_alone_count_as_a_change() {
        let repaired = Repaired::parse(&doc! { "numRemovedExtraIndexEntries": 1_i32 });

        assert!(repaired.changed_anything());
    }

    #[test]
    fn a_non_string_in_the_errors_array_is_dropped_rather_than_stringified() {
        let mut reply = sound();
        reply.insert("valid", false);
        reply.insert("errors", vec![bson::Bson::Int32(7)]);

        assert_eq!(
            Health::parse(&reply),
            Health::Damaged(Damage {
                errors: Vec::new(),
                missing_index_entries: 0,
            })
        );
    }
}
