use anyhow::Result;
use embedded_mongodb::{Cursor, bson::Document};
use std::time::{Duration, Instant};

pub struct Latency {
    pub median: Duration,
    pub mean: Duration,
    pub worst: Duration,
    pub matches: usize,
}

/// Drains a cursor and counts what came back, without holding the results. A query that
/// matches a few hundred thousand documents would otherwise put more into this process than
/// the engine itself holds, and the peak-RSS numbers would be measuring the client.
pub fn drain(cursor: Cursor<'_, Document>) -> Result<usize> {
    let mut matched = 0;
    for document in cursor {
        document?;
        matched += 1;
    }
    Ok(matched)
}

pub fn time<T>(action: impl FnOnce() -> Result<T>) -> Result<(T, Duration)> {
    let started = Instant::now();
    let value = action()?;
    Ok((value, started.elapsed()))
}

/// Runs `query` `iterations` times after a warm-up and summarises the distribution. The
/// warm-up matters more than usual here: the first execution of a query shape pays for plan
/// selection and for faulting the pages it touches into the WiredTiger cache.
pub fn latency(iterations: usize, query: impl Fn() -> Result<usize>) -> Result<Latency> {
    let mut matches = 0;
    for _ in 0..3 {
        matches = query()?;
    }

    let mut samples = Vec::with_capacity(iterations);
    for _ in 0..iterations {
        let (_, elapsed) = time(&query)?;
        samples.push(elapsed);
    }
    samples.sort_unstable();

    let total: Duration = samples.iter().sum();
    let median = samples
        .get(samples.len() / 2)
        .copied()
        .unwrap_or(Duration::ZERO);
    let worst = samples.last().copied().unwrap_or(Duration::ZERO);
    Ok(Latency {
        median,
        mean: total / samples.len().max(1) as u32,
        worst,
        matches,
    })
}

pub fn megabytes(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}
