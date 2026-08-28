//! SIGKILL, at the three moments an Android process is most likely to be reclaimed: mid-write,
//! mid-index-build, and mid-`close()`.

use crate::probe::{Role, harness::Child, index::SAMPLED_BUCKETS, outcome::Outcome, scratch};
use std::{thread, time::Duration};

/// Writes a probe waits to see acknowledged before it pulls the plug. Small on purpose: the
/// engine acknowledges tens of thousands of unjournalled inserts a second, so this lands the
/// kill inside the very first journal flush window, where the least is durable.
const ACKS_BEFORE_KILL: i64 = 200;

/// The same for journalled writes, which pay for a flush each and are therefore slower.
const JOURNALLED_ACKS_BEFORE_KILL: i64 = 50;

/// Documents loaded before `close()`. Enough that the shutdown checkpoint has real work to do
/// and the kill lands inside it rather than after.
const LOADED_DOCUMENTS: i64 = 120_000;

/// Documents loaded before the index build. Larger than the collection the `close()` probe
/// uses, because the build has to stay comfortably longer than [`INTO_THE_PHASE`] on a machine
/// faster than the one this was written on — otherwise the probe would kill a build that had
/// already finished and stop testing anything.
const INDEXED_DOCUMENTS: i64 = 400_000;

/// How long a phase is allowed to run before the kill. Long enough to be inside the work,
/// nowhere near long enough to be at its end.
const INTO_THE_PHASE: Duration = Duration::from_millis(150);

/// How long an unjournalled writer keeps going before the kill. The engine flushes its journal
/// every 100ms, so this is twenty flushes worth of margin: whatever a probe still loses after
/// waiting this long is a real loss window and not a race with the first flush.
const PAST_THE_FLUSH: Duration = Duration::from_secs(2);

/// Killed as early as the probe can manage, when the least is durable. The directory may come
/// back with nothing in it at all — an unjournalled insert into a collection that did not exist
/// yet loses the implicit `create` along with the documents — so nothing is claimed here about
/// what survived. What is claimed is that the directory still works: the verifier reaching
/// `closed` means the reopen succeeded and every catalog, aggregation, count and `validate`
/// command it runs on the way answered rather than failed.
#[test]
fn reopens_after_sigkill_during_insert() {
    let reopened = kill_during_insert(Role::Insert, ACKS_BEFORE_KILL, Duration::ZERO);

    assert_eq!(
        reopened.get("closed"),
        "ok",
        "the reopened directory did not survive being read\n{}",
        reopened.transcript()
    );
}

/// The realistic Android case: writes acknowledged without `j: true`, then the process goes
/// away. The three assertions are one claim — `_id`s start at zero, run unbroken, and reach at
/// least [`ACKS_BEFORE_KILL`] — which together say that every write acknowledged a full
/// [`PAST_THE_FLUSH`] before the kill came back. The transcript records how much of the tail
/// did not, which is the number that decides how an app on this engine has to write.
#[test]
fn unjournalled_writes_older_than_the_flush_interval_survive_sigkill() {
    let reopened = kill_during_insert(Role::Insert, ACKS_BEFORE_KILL, PAST_THE_FLUSH);

    assert_eq!(reopened.number("lowest_id"), 0, "{}", reopened.transcript());
    assert_eq!(
        reopened.number("holes"),
        0,
        "recovery left gaps in the _id sequence, so it did not replay a prefix of the writes\n{}",
        reopened.transcript()
    );
    assert!(
        reopened.number("count") >= ACKS_BEFORE_KILL,
        "only {} documents came back, fewer than the {ACKS_BEFORE_KILL} acknowledged a full \
         {PAST_THE_FLUSH:?} before the kill\n{}",
        reopened.number("count"),
        reopened.transcript()
    );
}

/// Whether the kill damaged the catalog. All three routes into it have to agree that the
/// collection is intact: it is listed, it has its `_id_` index, and `validate` finds no errors.
#[test]
fn catalog_survives_sigkill_during_insert() {
    let reopened = kill_during_insert(Role::Insert, ACKS_BEFORE_KILL, PAST_THE_FLUSH);

    assert_eq!(reopened.get("collections"), "records");
    assert_eq!(reopened.get("indexes"), "_id_");
    assert_eq!(reopened.get("validation_errors"), "");
}

#[test]
fn journalled_writes_survive_sigkill() {
    let reopened = kill_during_insert(
        Role::InsertJournaled,
        JOURNALLED_ACKS_BEFORE_KILL,
        Duration::ZERO,
    );

    assert_eq!(
        reopened.number("acknowledged_missing"),
        0,
        "a write acknowledged under j:true did not come back after the kill\n{}",
        reopened.transcript()
    );
}

/// The documents have to be all there and undamaged, whatever became of the index.
///
/// The load journals its last batch, so every document was durable before the build started;
/// none of them may go missing because the build that was reading them was killed. `validate`
/// runs over the `_id_` index that survives, so it is checking something real here.
#[test]
fn documents_survive_sigkill_during_index_build() {
    let reopened = kill_during_index_build();

    assert_eq!(reopened.number("count"), INDEXED_DOCUMENTS);
    assert_eq!(reopened.get("valid"), "true", "{}", reopened.transcript());
    assert_eq!(
        reopened.get("validation_errors"),
        "",
        "validate found damage in the collection the killed build was reading\n{}",
        reopened.transcript()
    );
}

/// The question this probe exists for: can a killed build leave an index that is half-built and
/// still used to answer queries — returning fewer documents than are really there?
///
/// It cannot, because the entries never survive the next open. `createIndexes` here is a
/// single-phase build, so an unfinished one is recorded in the durable catalog with no build
/// UUID, and `catalog_repair::reconcileCatalogAndIdents` drops both the catalog entry and its
/// table during startup. `listIndexes` therefore reports only `_id_`, the planner has nothing
/// to reach for, and a hint onto the vanished index is refused. The caller has to reissue the
/// `createIndexes` — which `reopened::an_index_can_be_created_on_a_reopened_database` shows
/// works — rather than silently inheriting a partial index.
#[test]
fn no_query_is_answered_from_the_index_a_killed_build_left() {
    let reopened = kill_during_index_build();

    assert_eq!(
        reopened.get("indexes"),
        "_id_",
        "the index a killed build left was kept instead of being dropped at startup\n{}",
        reopened.transcript()
    );

    // The counts first, not just the predicates: `all` answers an empty slice for a key that
    // was never reported, and every predicate below is true of an empty slice.
    let plans = reopened.all("indexed_plan");
    assert_eq!(
        plans.len(),
        SAMPLED_BUCKETS.len(),
        "the reopen reported {} access paths, not the {} it samples\n{}",
        plans.len(),
        SAMPLED_BUCKETS.len(),
        reopened.transcript()
    );
    assert!(
        plans.iter().all(|plan| plan == "COLLSCAN"),
        "the planner reached for the index left by a killed build ({plans:?}); the counts this \
         probe reports now have to be checked against it rather than assumed safe\n{}",
        reopened.transcript()
    );
    assert_eq!(
        reopened.all("hinted").len(),
        SAMPLED_BUCKETS.len(),
        "the reopen reported {} hint results, not the {} it samples\n{}",
        reopened.all("hinted").len(),
        SAMPLED_BUCKETS.len(),
        reopened.transcript()
    );
    assert!(
        reopened.all("hinted").iter().all(|hint| hint == "rejected"),
        "the engine accepted a hint onto the index a killed build left\n{}",
        reopened.transcript()
    );
}

#[test]
fn reopens_after_sigkill_during_close() {
    let directory = scratch::directory();
    let path = directory.path().join("database");

    let mut writer = Child::spawn(Role::InsertThenClose, &path, LOADED_DOCUMENTS);
    writer.wait_for_phase("closing");
    writer.kill().assert_killed().assert_never_reached("closed");

    let reopened = Child::spawn(Role::VerifyInserts, &path, LOADED_DOCUMENTS - 1).finish();
    reopened.assert_exited_cleanly().report();

    assert_eq!(reopened.get("valid"), "true", "{}", reopened.transcript());
    assert_eq!(
        reopened.number("acknowledged_missing"),
        0,
        "a journalled write was lost because close() never finished\n{}",
        reopened.transcript()
    );
}

/// Kills a writer once it has acknowledged `acks` writes and then kept going for `settle`, and
/// hands back what the reopen made of the directory it left behind.
fn kill_during_insert(role: Role, acks: i64, settle: Duration) -> Outcome {
    let directory = scratch::directory();
    let path = directory.path().join("database");

    let mut writer = Child::spawn(role, &path, 0);
    writer.wait_for_acks(acks);
    let acknowledged = writer.acks_over(settle);
    writer.kill().assert_killed();

    let reopened = Child::spawn(Role::VerifyInserts, &path, acknowledged).finish();
    reopened.assert_exited_cleanly().report();
    reopened
}

/// Kills a `createIndexes` in flight and hands back what the reopen made of it.
fn kill_during_index_build() -> Outcome {
    let directory = scratch::directory();
    let path = directory.path().join("database");

    let mut builder = Child::spawn(Role::BuildIndex, &path, INDEXED_DOCUMENTS);
    builder.wait_for_phase("building");
    thread::sleep(INTO_THE_PHASE);
    builder.kill().assert_killed().assert_never_reached("built");

    let reopened = Child::spawn(Role::VerifyIndex, &path, INDEXED_DOCUMENTS).finish();
    reopened.assert_exited_cleanly().report();
    reopened
}
