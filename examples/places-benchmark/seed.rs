use anyhow::{Context, Result};
use embedded_mongodb::bson::{Bson, Document, doc};
use flate2::read::GzDecoder;
use std::{fs::File, io::Read, path::Path};

pub struct Seed {
    pub documents: Vec<Document>,
    pub compressed_bytes: u64,
    pub raw_bytes: u64,
}

/// Reads the concatenated BSON stream `scripts/build-places-seed` writes.
pub fn load(path: &Path) -> Result<Seed> {
    let compressed_bytes = std::fs::metadata(path)
        .with_context(|| {
            format!(
                "{} is missing; run scripts/build-places-seed",
                path.display()
            )
        })?
        .len();

    let mut raw = Vec::new();
    GzDecoder::new(File::open(path)?).read_to_end(&mut raw)?;

    // `from_reader` consumes exactly one document, so the slice reader lands on the next one.
    let mut remaining = raw.as_slice();
    let mut documents = Vec::new();
    while !remaining.is_empty() {
        documents.push(Document::from_reader(&mut remaining)?);
    }

    Ok(Seed {
        raw_bytes: raw.len() as u64,
        compressed_bytes,
        documents,
    })
}

/// Degrees of longitude between successive replicas; 254 of them wrap the globe several times
/// without ever landing two copies of the same place on one point.
const SHIFT_DEGREES: f64 = 7.3;

/// One replica of a place, moved to a distinct point on the globe and given a distinct id.
/// Copy 0 is the extract itself, unchanged.
///
/// Replication is how the world-scale numbers get measured instead of extrapolated. The shift
/// matters: stacking every copy on the same coordinates would hand the 2dsphere index a key
/// distribution no real dataset has, and would make its build time and size meaningless.
pub fn replica(document: &Document, copy: usize) -> Result<Document> {
    if copy == 0 {
        return Ok(document.clone());
    }

    let identifier = document.get_str("_id").context("place has no string _id")?;
    let point = document
        .get_document("loc")
        .and_then(|location| location.get_array("coordinates"))
        .context("place has no GeoJSON point")?;
    let (Some(Bson::Double(longitude)), Some(Bson::Double(latitude))) =
        (point.first(), point.get(1))
    else {
        anyhow::bail!("place coordinates are not a pair of doubles");
    };

    let shift = copy as f64 * SHIFT_DEGREES;
    let longitude = (longitude + 180.0 + shift).rem_euclid(360.0) - 180.0;
    let latitude = (latitude + 85.0 + shift * 0.37).rem_euclid(170.0) - 85.0;

    let mut replica = document.clone();
    replica.insert("_id", format!("{identifier}:{copy}"));
    replica.insert(
        "loc",
        doc! { "type": "Point", "coordinates": [longitude, latitude] },
    );
    Ok(replica)
}
