use embedded_mongodb::{
    Client, Collection,
    bson::{Document, doc},
};

pub const DATABASE: &str = "places";
pub const COLLECTION: &str = "coffee";

/// O'Connell Bridge, Dublin: the point the demo app searches from.
const DUBLIN: [f64; 2] = [-6.2603, 53.3498];
const NEARBY_RADIUS_KM: f64 = 5.0;
const EARTH_RADIUS_KM: f64 = 6378.1;
const NEARBY_LIMIT: i64 = 50;

pub fn collection(client: &Client) -> Collection<'_, Document> {
    client.database(DATABASE).collection(COLLECTION)
}

pub fn geo_near(category: Option<&str>) -> Vec<Document> {
    let mut stage = doc! {
        "near": { "type": "Point", "coordinates": DUBLIN.to_vec() },
        "distanceField": "distance",
        "spherical": true,
    };
    if let Some(category) = category {
        stage.insert("query", doc! { "cat": category });
    }
    vec![doc! { "$geoNear": stage }, doc! { "$limit": NEARBY_LIMIT }]
}

/// The nearby search a collection scan can answer. `$geoNear` refuses to run at all without a
/// 2dsphere index, so an unindexed baseline has to be this instead: the same places within the
/// radius, but neither sorted by distance nor annotated with one.
pub fn geo_within() -> Document {
    let radius_radians = NEARBY_RADIUS_KM / EARTH_RADIUS_KM;
    doc! {
        "loc": {
            "$geoWithin": { "$centerSphere": [DUBLIN.to_vec(), radius_radians] },
        },
    }
}

pub fn text_search(term: &str) -> Document {
    doc! { "$text": { "$search": term } }
}

/// Unindexed stand-in for the text search: the same term, matched by a case-insensitive regex
/// that no index can serve.
pub fn name_regex(term: &str) -> Document {
    doc! { "name": { "$regex": term, "$options": "i" } }
}

/// The two indexes the demo app needs, in the order the benchmark builds them.
pub fn index_commands() -> [(&'static str, Document); 2] {
    [
        (
            "2dsphere on loc",
            doc! {
                "createIndexes": COLLECTION,
                "indexes": [{ "key": { "loc": "2dsphere" }, "name": "loc_2dsphere" }],
            },
        ),
        (
            "text on name + brand",
            doc! {
                "createIndexes": COLLECTION,
                "indexes": [{
                    "key": { "name": "text", "brand": "text" },
                    "name": "name_brand_text",
                }],
            },
        ),
    ]
}
