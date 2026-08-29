//! What a finished child said, and the assertions a probe makes about it.

use super::Role;
use std::{collections::HashMap, os::unix::process::ExitStatusExt, process::ExitStatus};

/// SIGKILL. Spelled out here rather than pulled in from `libc`, which this crate does not
/// depend on and which one constant does not justify adding.
const SIGKILL: i32 = 9;

/// Everything a finished child reported.
pub struct Outcome {
    role: Role,
    status: ExitStatus,
    results: HashMap<String, Vec<String>>,
    seen: Vec<String>,
}

impl Outcome {
    pub(super) fn new(
        role: Role,
        status: ExitStatus,
        results: HashMap<String, Vec<String>>,
        seen: Vec<String>,
    ) -> Self {
        Self {
            role,
            status,
            results,
            seen,
        }
    }

    /// The kill is what ended the child: not a panic, and not a clean exit that beat the probe
    /// to it.
    pub fn assert_killed(&self) -> &Self {
        assert_eq!(
            self.status.signal(),
            Some(SIGKILL),
            "{} was not killed; it exited with {:?}\n{}",
            self.role.as_str(),
            self.status.code(),
            self.transcript()
        );
        self
    }

    /// The child never got as far as `name`, which is how a probe shows the kill landed inside
    /// the phase it was aiming at rather than after it.
    pub fn assert_never_reached(&self, name: &str) -> &Self {
        let phase = format!("phase {name}");
        assert!(
            !self.seen.contains(&phase),
            "{} reached {name} before the kill\n{}",
            self.role.as_str(),
            self.transcript()
        );
        self
    }

    /// The engine took the process down with it.
    ///
    /// Pinning a defect, not blessing one: MongoDB turns a WiredTiger panic into `fassert`,
    /// and `fassert` calls `abort()`. A caller gets no `Error` and no chance to react. If this
    /// assertion ever starts failing the engine has learned to report the failure instead,
    /// and the probe that uses it should become an ordinary error assertion.
    /// Whether the engine took the process down with it.
    ///
    /// A predicate rather than an assertion, because the probes that use it are pinning
    /// defects: each one has its own message explaining that a failure here means the engine
    /// was repaired, and a shared assertion could not say that.
    ///
    /// Any death counts, not `SIGABRT` alone. The defect is that the caller gets no `Error`
    /// and the process goes down; which signal carries that is the platform's business. On
    /// Android this probe died without `SIGABRT` -- bionic routes a crash through its own
    /// handler -- and an assertion naming one signal reported the engine as repaired when it
    /// had only died differently.
    ///
    /// The exchange is precision: a panic in the child now reads as the defect too. That is
    /// worth it because the failure this pins is the process dying, and the alternative was a
    /// probe that only recognises one platform's way of dying. A clean exit remains the one
    /// outcome meaning the engine learned to report the failure -- which is what the probes
    /// using this assert on, and why each names `ending()` in its message.
    pub fn was_aborted(&self) -> bool {
        !self.status.success()
    }

    /// How the child ended, for a message that has to explain an unexpected outcome.
    pub fn ending(&self) -> String {
        match (self.status.code(), self.status.signal()) {
            (_, Some(signal)) => format!("killed by signal {signal}"),
            (Some(code), None) => format!("exited with code {code}"),
            (None, None) => "ended in a way this platform does not report".to_owned(),
        }
    }

    /// The child ran to the end on its own: no panic, no signal, no abort.
    pub fn assert_exited_cleanly(&self) -> &Self {
        assert!(
            self.status.success(),
            "{} exited with code {:?} and signal {:?}\n{}",
            self.role.as_str(),
            self.status.code(),
            self.status.signal(),
            self.transcript()
        );
        self
    }

    pub fn get(&self, key: &str) -> &str {
        let Some(value) = self.results.get(key).and_then(|values| values.first()) else {
            panic!(
                "{} reported no {key}\n{}",
                self.role.as_str(),
                self.transcript()
            );
        };
        value
    }

    pub fn number(&self, key: &str) -> i64 {
        let raw = self.get(key);
        let Ok(value) = raw.parse::<i64>() else {
            panic!(
                "{} reported {key}={raw}, which is not a number",
                self.role.as_str()
            );
        };
        value
    }

    pub fn all(&self, key: &str) -> &[String] {
        self.results.get(key).map_or(&[], Vec::as_slice)
    }

    /// Prints everything the child said. Probes call this on the way past so a passing run
    /// still leaves its numbers behind — the evidence is the point, not the green tick.
    pub fn report(&self) -> &Self {
        eprintln!("{}", self.transcript());
        self
    }

    pub fn transcript(&self) -> String {
        transcript(self.role, &self.seen)
    }
}

pub(super) fn transcript(role: Role, seen: &[String]) -> String {
    format!("--- {} said ---\n{}", role.as_str(), seen.join("\n"))
}
