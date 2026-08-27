// cargo run --release --example places-benchmark -- [seed.bson.gz] [data-dir] [copies] [--force]
// cargo run --release --example places-benchmark -- --cold-open <data-dir> [query-repeats]
//
// Measures what the Overture coffee seed costs the embedded engine: insert throughput, index
// build time, on-disk footprint, peak resident memory and the latency of the three queries the
// Android demo app runs. Written as an example rather than a criterion bench because every
// number here is a property of one full load -- peak RSS, directory size and cold-open time
// are meaningless once a harness has replayed the workload a few hundred times into the same
// process and the same directory.

mod cold;
mod datadir;
mod measure;
mod queries;
mod report;
mod rss;
mod seed;

use anyhow::{Context, Result};
use datadir::footprint;
use embedded_mongodb::Client;
use measure::{drain, latency, time};
use report::{bytes, heading, latency_row, millis, row};
use std::path::{Path, PathBuf};

/// Big enough that per-command overhead disappears, small enough that one failed batch is a
/// readable error and the peak-RSS sampler still sees several points inside the load.
const BATCH_SIZE: usize = 500;
const QUERY_ITERATIONS: usize = 50;
/// A replicated run answers the same queries over hundreds of times as many documents, so it
/// samples fewer times to keep the whole benchmark to a few minutes.
const REPLICATED_QUERY_ITERATIONS: usize = 5;
/// Enough repeats of the app's search in the cold child to show whether serving queries settles
/// at the WiredTiger cache size or keeps climbing. The reported open time is still the first
/// query's alone.
const COLD_OPEN_REPEATS: usize = 5;
/// Places in the whole 2026-08-19.0 release, for the extrapolation at the end of the report.
const WORLD_PLACES: f64 = 1_315_781.0;

fn main() -> Result<()> {
    let mut arguments: Vec<String> = std::env::args().skip(1).collect();
    // --force may sit anywhere; everything else is positional.
    let unrecognised = match arguments.iter().any(|argument| argument == "--force") {
        true => datadir::Unrecognised::Delete,
        false => datadir::Unrecognised::Refuse,
    };
    arguments.retain(|argument| argument != "--force");
    let mut arguments = arguments.into_iter();
    let first = arguments.next();
    if first.as_deref() == Some("--cold-open") {
        let directory = arguments
            .next()
            .context("--cold-open needs a data directory")?;
        let repeats = match arguments.next() {
            Some(repeats) => repeats
                .parse()
                .context("repeats must be a positive integer")?,
            None => 1,
        };
        return cold::open(Path::new(&directory), repeats);
    }

    let seed_path = first.map_or_else(default_seed, PathBuf::from);
    let data_directory = arguments.next().map_or_else(default_data, PathBuf::from);
    let copies = match arguments.next() {
        Some(copies) => copies
            .parse()
            .context("copies must be a positive integer")?,
        None => 1,
    };
    benchmark(&seed_path, &data_directory, copies, unrecognised)
}

fn benchmark(
    seed_path: &Path,
    data_directory: &Path,
    copies: usize,
    unrecognised: datadir::Unrecognised,
) -> Result<()> {
    // A surviving directory would turn the load into an update and would let the cold-open
    // measurement read data this run never wrote.
    datadir::prepare(data_directory, unrecognised)?;

    let iterations = if copies > 1 {
        REPLICATED_QUERY_ITERATIONS
    } else {
        QUERY_ITERATIONS
    };
    let sampler = rss::Sampler::start();
    let seed = seed::load(seed_path)?;
    let documents = seed.documents.len() * copies;

    heading("dataset");
    row("seed", seed_path.display().to_string());
    row(
        "documents",
        match copies {
            1 => documents.to_string(),
            copies => format!("{documents} ({copies} copies of the extract)"),
        },
    );
    row("bson, uncompressed", bytes(seed.raw_bytes));
    row("bson, gzip -9", bytes(seed.compressed_bytes));

    let (client, open_time) = time(|| Ok(Client::new(data_directory)?))?;
    let places = queries::collection(&client);
    // Replicas are built a batch at a time. Materialising 1.3 million documents up front would
    // put more into this process than the engine holds and would swamp the RSS measurement.
    let (_, insert_time) = time(|| {
        for copy in 0..copies {
            for batch in seed.documents.chunks(BATCH_SIZE) {
                let batch = batch
                    .iter()
                    .map(|place| seed::replica(place, copy))
                    .collect::<Result<Vec<_>>>()?;
                places.insert_many(&batch)?;
            }
        }
        Ok(())
    })?;

    let insert_peak = sampler.take_peak();

    // Both baselines have to run before the indexes exist: once a 2dsphere index is on `loc`
    // the planner will use it for `$geoWithin` too, and the scan cost is no longer observable.
    let scanned_geo = latency(iterations, || drain(places.find(queries::geo_within())?))?;
    let scanned_text = latency(iterations, || {
        Ok(places
            .find(queries::name_regex("coffee"))?
            .try_collect()?
            .len())
    })?;

    let scan_peak = sampler.take_peak();

    let database = client.database(queries::DATABASE);
    let mut index_times = Vec::new();
    for (label, command) in queries::index_commands() {
        let (_, elapsed) = time(|| Ok(database.run_command(&command)?))?;
        index_times.push((label, elapsed));
    }
    let index_peak = sampler.take_peak();

    heading("load");
    row("open an empty directory", millis(open_time));
    row(
        &format!("insert {documents} docs, batches of {BATCH_SIZE}"),
        format!(
            "{} ({:.0} docs/s)",
            millis(insert_time),
            documents as f64 / insert_time.as_secs_f64()
        ),
    );
    for (label, elapsed) in &index_times {
        row(&format!("build index: {label}"), millis(*elapsed));
    }

    let indexed_geo = latency(iterations, || drain(places.find(queries::geo_within())?))?;
    let near = latency(iterations, || {
        Ok(places
            .aggregate(queries::geo_near(None))?
            .try_collect()?
            .len())
    })?;
    let near_filtered = latency(iterations, || {
        drain(places.aggregate(queries::geo_near(Some("coffee_shop")))?)
    })?;
    let text = latency(iterations, || {
        Ok(places
            .find(queries::text_search("coffee"))?
            .try_collect()?
            .len())
    })?;
    let query_peak = sampler.take_peak();

    heading(&format!("queries ({iterations} runs each)"));
    latency_row("$geoWithin 5 km, collection scan", &scanned_geo);
    latency_row("$geoWithin 5 km, 2dsphere", &indexed_geo);
    latency_row("$geoNear Dublin, limit 50", &near);
    latency_row("$geoNear + cat filter, limit 50", &near_filtered);
    latency_row("name regex /coffee/i, collection scan", &scanned_text);
    latency_row("$text 'coffee', text index", &text);

    client.close()?;
    let close_peak = sampler.take_peak();
    let on_disk = footprint(data_directory)?;

    heading("footprint");
    row("data directory after clean close", bytes(on_disk.total));
    row("  of which preallocated journal", bytes(on_disk.journal));
    row(
        "  of which tables and catalog",
        bytes(on_disk.total - on_disk.journal),
    );
    row("peak RSS inserting", bytes(insert_peak));
    row("peak RSS scanning without indexes", bytes(scan_peak));
    row("peak RSS building indexes", bytes(index_peak));
    row("peak RSS querying with indexes", bytes(query_peak));
    row("peak RSS closing (final checkpoint)", bytes(close_peak));
    row("peak RSS, whole process (VmHWM)", bytes(rss::peak_rss()?));

    // A cold open has to happen in a process that has never held this data: reopening in the
    // process that just wrote it would measure a warm allocator and an already-grown heap.
    heading("cold open, fresh process");
    cold::spawn(data_directory, COLD_OPEN_REPEATS)?;

    // A replicated run has measured the whole release rather than guessed at it.
    if copies == 1 {
        report::extrapolation(report::Extrapolation {
            factor: WORLD_PLACES / documents as f64,
            seed_bytes: seed.compressed_bytes,
            insert: insert_time,
            indexes: &index_times,
            on_disk: &on_disk,
        });
    }
    Ok(())
}

fn default_seed() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(".cache/places/ireland.bson.gz")
}

fn default_data() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(".cache/places-benchmark")
}
