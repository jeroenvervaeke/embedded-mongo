use anyhow::{Context, Result};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

/// Highest resident set size seen since the last [`Sampler::take_peak`].
///
/// `VmHWM` is a process-lifetime high-water mark that cannot be reset without privileges, so
/// per-phase peaks have to come from polling `VmRSS`. The interval is short relative to the
/// phases being measured, but it is still sampling: a spike narrower than the interval can be
/// missed, which is why [`peak_rss`] reports the kernel's own mark as a cross-check.
/// Short relative to every phase measured here, and cheap: one small `/proc` read.
const INTERVAL: Duration = Duration::from_millis(2);

pub struct Sampler {
    peak: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Sampler {
    pub fn start() -> Self {
        let peak = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let peak = Arc::clone(&peak);
            let stop = Arc::clone(&stop);
            thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    if let Ok(resident) = read_status_field("VmRSS") {
                        peak.fetch_max(resident, Ordering::Relaxed);
                    }
                    thread::sleep(INTERVAL);
                }
            })
        };
        Self {
            peak,
            stop,
            thread: Some(thread),
        }
    }

    /// Returns the peak observed so far and starts a fresh phase from the current RSS, so a
    /// later phase is never credited with an earlier phase's high-water mark.
    pub fn take_peak(&self) -> u64 {
        let current = read_status_field("VmRSS").unwrap_or(0);
        self.peak.swap(current, Ordering::Relaxed)
    }
}

impl Drop for Sampler {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The kernel's own peak resident set size for this process, in bytes.
pub fn peak_rss() -> Result<u64> {
    read_status_field("VmHWM")
}

fn read_status_field(field: &str) -> Result<u64> {
    let status = std::fs::read_to_string("/proc/self/status")?;
    let value = status
        .lines()
        .find_map(|line| line.strip_prefix(field)?.strip_prefix(':'))
        .with_context(|| format!("/proc/self/status has no {field} line"))?;
    let kilobytes = value
        .split_whitespace()
        .next()
        .with_context(|| format!("{field} line has no value"))?;
    Ok(kilobytes.parse::<u64>()? * 1024)
}
