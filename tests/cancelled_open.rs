//! That an open nobody waited for closes the engine it started.
//!
//! This process opens one engine at a time -- a second `Client::open` on a live runtime is
//! refused -- so an open whose future is dropped after the engine came up, and which then had
//! nothing left to close it, would not merely leak: it would leave the process unable to open a
//! database again for as long as it runs. There is no recovering from it and nothing to see
//! except every later open failing.
//!
//! What rules it out is that the handle on the workers exists before the wait for them does, so
//! dropping the future drops the handle and abandons the pool. Cancelling mid-open is the
//! deterministic way to reach that path; the narrower race -- a future dropped just after the
//! open reported success -- ends at the same handle.

#[path = "scratch/mod.rs"]
mod scratch;

use embedded_mongodb::{Client, bson::doc};
use std::{
    path::Path,
    time::{Duration, Instant},
};

#[tokio::test(flavor = "multi_thread")]
async fn an_open_nobody_waited_for_closes_the_engine_it_started() {
    let temporary = scratch::directory("cancelled-open-");
    let path = temporary.path().join("database");

    // Shorter than an open can possibly take, so the future is dropped with the engine still
    // starting up on its worker thread.
    let abandoned = tokio::time::timeout(Duration::from_millis(1), Client::new(&path)).await;
    assert!(
        abandoned.is_err(),
        "the open finished inside a millisecond, so this proves nothing about cancelling one"
    );

    let client = reopen(&path).await;
    let ping = client
        .run_command("admin", doc! { "ping": 1 })
        .await
        .expect("the reopened engine should answer");
    assert_eq!(ping.get_f64("ok").expect("an ok field"), 1.0);
    client.close().await.expect("closing the reopened client");
}

/// Opens again, retrying while the abandoned engine finishes closing.
///
/// The close is not instant and nothing reports it: the workers of the abandoned open have to
/// find the queue dry and let go, and the last of them closes the engine on its way out. So
/// this waits rather than asserting on the first attempt -- but it waits for something that
/// does happen, and fails the test outright if it does not.
async fn reopen(path: &Path) -> Client {
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut refused = None;
    while Instant::now() < deadline {
        match Client::new(path).await {
            Ok(client) => return client,
            Err(error) => {
                refused = Some(error);
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    }
    panic!(
        "the engine an abandoned open started was never closed, so this process can no longer \
         open a database: {refused:?}"
    );
}
