//! Where one mover waits for another to reach the same knob.
//!
//! A test of the serialisation is worthless if it passes because nothing raced, so the fake
//! engine does not leave the interleaving to the scheduler: the first mover into the index-build
//! knob is held there until a second one arrives. Two movers that are *not* serialised therefore
//! interleave every run rather than on an unlucky one, and two that are cannot both arrive at
//! all -- which is what the timeout below is for.

use std::{
    sync::{Condvar, Mutex, PoisonError},
    time::Duration,
};

pub(crate) struct Rendezvous {
    arrived: Mutex<usize>,
    another: Condvar,
}

impl Rendezvous {
    /// How many movers a test runs at the floors, and so how many have to arrive before the
    /// first one is let go.
    const MOVERS: usize = 2;

    /// Long enough that a mover which is *able* to arrive certainly has. It is paid in full by a
    /// run where the movers are serialised and the second one can never arrive, which is the
    /// price of a test that fails without the serialisation every time rather than whenever the
    /// scheduler happens to cooperate.
    const PATIENCE: Duration = Duration::from_millis(500);

    pub(crate) fn new() -> Self {
        Self {
            arrived: Mutex::new(0),
            another: Condvar::new(),
        }
    }

    pub(crate) fn wait_for_another(&self) {
        let mut arrived = self.arrived.lock().unwrap_or_else(PoisonError::into_inner);
        *arrived += 1;
        self.another.notify_all();
        if *arrived >= Self::MOVERS {
            return;
        }
        let _waited = self
            .another
            .wait_timeout_while(arrived, Self::PATIENCE, |arrived| *arrived < Self::MOVERS);
    }
}
