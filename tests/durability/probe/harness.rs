//! The parent half: spawn a role, listen to it, take it away, read what is left.

use super::{
    Role, child,
    outcome::{Outcome, transcript},
};
use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Write},
    path::Path,
    process::{Command, Stdio},
    sync::mpsc::{Receiver, RecvTimeoutError, channel},
    thread,
    time::{Duration, Instant},
};

/// How long a probe waits for a line it expects. Generous: the first open of a directory pays
/// for engine startup, and the machine may be running several probes at once.
const PATIENCE: Duration = Duration::from_secs(120);

pub struct Child {
    role: Role,
    process: std::process::Child,
    /// Lines the child has written, delivered by a reader thread. Reading through a channel
    /// rather than straight off the pipe is what lets a probe put a deadline on a child that
    /// has wedged, instead of blocking on it until the whole run times out.
    lines: Receiver<String>,
    seen: Vec<String>,
    /// Highest sequence this process has seen acknowledged. A running maximum over a stream
    /// that is consumed as it arrives, so there is nothing to recompute it from later.
    highest_ack: i64,
}

impl Child {
    /// Re-executes this test binary in child mode.
    ///
    /// The child is this same binary rather than an `examples/` helper because
    /// `cargo test --test durability` does not build example targets: the helper would be
    /// missing, or stale, exactly when the suite runs.
    pub fn spawn(role: Role, directory: &Path, argument: i64) -> Self {
        let executable = std::env::current_exe().expect("the test binary knows its own path");
        let mut process = Command::new(executable)
            .args([
                "--exact",
                child::ENTRY_POINT,
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(child::ROLE, role.as_str())
            .env(child::DIRECTORY, directory)
            .env(child::ARGUMENT, argument.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap_or_else(|error| panic!("spawning {} failed: {error}", role.as_str()));

        let stdout = process
            .stdout
            .take()
            .expect("stdout was piped one statement ago");
        let (sender, lines) = channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { return };
                let payload = line
                    .rsplit_once(child::MARK)
                    .map_or(line.as_str(), |(_, rest)| rest);
                if sender.send(payload.to_owned()).is_err() {
                    return;
                }
            }
        });

        Self {
            role,
            process,
            lines,
            seen: Vec::new(),
            highest_ack: -1,
        }
    }

    /// Blocks until the child announces `name`.
    pub fn wait_for_phase(&mut self, name: &str) {
        let wanted = format!("phase {name}");
        let expected = wanted.clone();
        self.read_until(&wanted, move |child| {
            child.seen.last().is_some_and(|line| *line == expected)
        });
    }

    /// Blocks until the child has acknowledged at least `wanted` writes, and answers with the
    /// highest sequence this process actually saw acknowledged — a lower bound on what the
    /// engine had committed to before the kill.
    pub fn wait_for_acks(&mut self, wanted: i64) -> i64 {
        self.read_until(&format!("{wanted} acknowledged writes"), move |child| {
            child.highest_ack + 1 >= wanted
        });
        self.highest_ack
    }

    /// Keeps listening for `duration` before answering, so a probe can put the kill well past
    /// the engine's journal flush interval instead of inside the first one.
    pub fn acks_over(&mut self, duration: Duration) -> i64 {
        let deadline = Instant::now() + duration;
        while let Some(remaining) = deadline
            .checked_duration_since(Instant::now())
            .filter(|left| !left.is_zero())
        {
            match self.lines.recv_timeout(remaining) {
                Ok(line) => self.observe(line),
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => panic!(
                    "{} stopped writing before the probe was ready to kill it\n{}",
                    self.role.as_str(),
                    transcript(self.role, &self.seen)
                ),
            }
        }
        self.highest_ack
    }

    /// SIGKILL, then collect whatever the child had already said. Android's process death
    /// gives no warning, no unwinding and no destructors, and neither does this.
    pub fn kill(mut self) -> Outcome {
        self.terminate();
        self.finish()
    }

    /// Lets a `hold-open` child go.
    pub fn release(&mut self) {
        let Some(mut stdin) = self.process.stdin.take() else {
            panic!("{} was already released", self.role.as_str());
        };
        stdin
            .write_all(b"go\n")
            .unwrap_or_else(|error| panic!("releasing {} failed: {error}", self.role.as_str()));
    }

    /// Waits for the child to run out of things to say, then reaps it.
    pub fn finish(mut self) -> Outcome {
        loop {
            match self.lines.recv_timeout(PATIENCE) {
                Ok(line) => self.observe(line),
                Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) => {
                    self.terminate();
                    panic!(
                        "{} went quiet for {PATIENCE:?} without finishing\n{}",
                        self.role.as_str(),
                        transcript(self.role, &self.seen)
                    );
                }
            }
        }

        let mut results: HashMap<String, Vec<String>> = HashMap::new();
        for line in &self.seen {
            let Some((key, value)) = line
                .strip_prefix("result ")
                .and_then(|entry| entry.split_once('='))
            else {
                continue;
            };
            results
                .entry(key.to_owned())
                .or_default()
                .push(value.to_owned());
        }

        let status = self
            .process
            .wait()
            .unwrap_or_else(|error| panic!("waiting for {} failed: {error}", self.role.as_str()));
        Outcome::new(self.role, status, results, self.seen)
    }

    fn terminate(&mut self) {
        self.process
            .kill()
            .unwrap_or_else(|error| panic!("killing {} failed: {error}", self.role.as_str()));
    }

    /// Records a line and, when it is an acknowledgement, moves the running maximum along.
    fn observe(&mut self, line: String) {
        if let Some(sequence) = line
            .strip_prefix("ack ")
            .and_then(|sequence| sequence.parse::<i64>().ok())
        {
            self.highest_ack = self.highest_ack.max(sequence);
        }
        self.seen.push(line);
    }

    fn read_until(&mut self, wanted: &str, accept: impl Fn(&Self) -> bool) {
        loop {
            match self.lines.recv_timeout(PATIENCE) {
                Ok(line) => {
                    self.observe(line);
                    if accept(self) {
                        return;
                    }
                }
                Err(RecvTimeoutError::Disconnected) => panic!(
                    "{} ended before it reached `{wanted}`\n{}",
                    self.role.as_str(),
                    transcript(self.role, &self.seen)
                ),
                Err(RecvTimeoutError::Timeout) => {
                    self.terminate();
                    panic!(
                        "{} went quiet for {PATIENCE:?} while waiting for `{wanted}`\n{}",
                        self.role.as_str(),
                        transcript(self.role, &self.seen)
                    );
                }
            }
        }
    }
}
