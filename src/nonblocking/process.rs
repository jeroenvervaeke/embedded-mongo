use super::Client;
use crate::{FreeDiskFloor, ReportedFloors, Result};

/// The async face of [`blocking::ProcessLimits`](crate::blocking::ProcessLimits): the floors
/// are the process's rather than this client's, and everything documented there -- what a
/// floor outlives, why every open re-establishes it, what a half-moved pair would mean --
/// holds unchanged. Each call here runs the blocking handle's logic on a worker thread, under
/// the same process-wide lock, so async and blocking movers serialize against each other.
/// Unlike every other command this crate awaits, these two are **not** cancelled by dropping
/// their future. A floor is a pair of server parameters set by two commands, and abandoning the
/// pair half-way is the very state
/// [`Error::FreeDiskFloorNotRestored`](crate::Error::FreeDiskFloorNotRestored) exists to report:
/// an engine left on floors nobody chose. Dropping one of these futures stops you waiting; the
/// pair still completes.
#[derive(Clone, Copy)]
pub struct ProcessLimits<'client> {
    client: &'client Client,
}

impl<'client> ProcessLimits<'client> {
    pub(super) fn new(client: &'client Client) -> Self {
        Self { client }
    }

    /// Applies `floor` to the engine this client has open; see
    /// [`blocking::ProcessLimits::set_free_disk_floor`](crate::blocking::ProcessLimits::set_free_disk_floor)
    /// for the put-back-on-failure contract and
    /// [`Error::FreeDiskFloorNotRestored`](crate::Error::FreeDiskFloorNotRestored).
    pub async fn set_free_disk_floor(&self, floor: FreeDiskFloor) -> Result<()> {
        self.client
            .engine()
            .run(move |client| client.process_limits().set_free_disk_floor(floor))
            .await
    }

    /// What the engine says the two floors are now; see
    /// [`blocking::ProcessLimits::free_disk_floors`](crate::blocking::ProcessLimits::free_disk_floors).
    pub async fn free_disk_floors(&self) -> Result<ReportedFloors> {
        self.client
            .engine()
            .run(|client| client.process_limits().free_disk_floors())
            .await
    }
}
