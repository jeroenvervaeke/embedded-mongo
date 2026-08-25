//! Engine surface an embedded document store has to keep working.
//!
//! The native library is built with whole subsystems removed — SBE, sharding, replication,
//! authentication, the network stack — and almost all of that code is reachable only through
//! startup registration, so the linker cannot tell us when we have cut too much. This test is
//! what tells us. Everything here is exercised through the real engine; a section that stops
//! working names the feature that a build change took away.
//!
//! One test function, not many: the engine is a process-global singleton, and `cargo test`
//! would otherwise open several of them at once.

use embedded_mongodb::{
    Client,
    bson::{Bson, Document, doc, oid::ObjectId},
};

/// Every document the typed collection helpers are used with.
#[derive(Debug, serde::Deserialize, serde::Serialize)]
struct Row {
    #[serde(rename = "_id", default, skip_serializing_if = "Option::is_none")]
    id: Option<ObjectId>,
    name: String,
    score: i32,
}

/// A named group of assertions, run against one shared client.
type Section = (&'static str, fn(&Client));

/// Named so a crash inside the engine, which aborts the process without unwinding, still
/// tells us which section it died in.
const SECTIONS: &[Section] = &[
    ("crud_commands", crud_commands),
    ("query_operators", query_operators),
    ("indexes", indexes),
    ("index_is_actually_used", index_is_actually_used),
    ("collation", collation),
    ("aggregation_stages", aggregation_stages),
    ("aggregation_expressions", aggregation_expressions),
    ("aggregation_output_stages", aggregation_output_stages),
    ("bson_types", bson_types),
    ("collection_administration", collection_administration),
    ("error_paths", error_paths),
    ("embedded_identity", embedded_identity),
];

#[test]
fn engine_features_survive_the_build_cuts() {
    let temporary = tempfile::tempdir().unwrap();
    let client = Client::new(temporary.path().join("database")).unwrap();

    for (name, section) in SECTIONS {
        eprintln!("--- {name}");
        section(&client);
    }

    client.close().unwrap();
}

/// insert / update / delete / findAndModify, the write path that does not go through the
/// typed helpers.
fn crud_commands(client: &Client) {
    let db = client.database("crud");

    db.run_command(&doc! {
        "insert": "people",
        "documents": [
            { "_id": 1, "name": "Ada", "score": 10, "tags": ["x", "y"] },
            { "_id": 2, "name": "Grace", "score": 20, "tags": ["y"] },
            { "_id": 3, "name": "Linus", "score": 30, "tags": [] },
        ],
    })
    .unwrap();

    let updated = db
        .run_command(&doc! {
            "update": "people",
            "updates": [
                { "q": { "_id": 1 }, "u": { "$set": { "score": 15 } } },
                { "q": { "score": { "$gte": 20 } }, "u": { "$inc": { "score": 1 } }, "multi": true },
                { "q": { "_id": 9 }, "u": { "$set": { "name": "Upserted", "score": 0 } }, "upsert": true },
            ],
        })
        .unwrap();
    assert_eq!(
        updated.get_i32("n").unwrap(),
        4,
        "update matched wrong count"
    );
    assert_eq!(
        updated.get_array("upserted").unwrap().len(),
        1,
        "upsert did not report an upserted id"
    );

    // Array update operators are a separate code path from $set/$inc.
    db.run_command(&doc! {
        "update": "people",
        "updates": [
            { "q": { "_id": 1 }, "u": { "$push": { "tags": "z" }, "$unset": { "nothing": "" } } },
            { "q": { "_id": 2 }, "u": { "$addToSet": { "tags": "y" } } },
            { "q": { "_id": 3 }, "u": [{ "$set": { "score": { "$add": ["$score", 100] } } }] },
        ],
    })
    .unwrap();

    let modified = db
        .run_command(&doc! {
            "findAndModify": "people",
            "query": { "_id": 2 },
            "update": { "$set": { "name": "Grace Hopper" } },
            "new": true,
        })
        .unwrap();
    assert_eq!(
        modified
            .get_document("value")
            .unwrap()
            .get_str("name")
            .unwrap(),
        "Grace Hopper"
    );

    let deleted = db
        .run_command(&doc! {
            "delete": "people",
            "deletes": [{ "q": { "_id": 9 }, "limit": 1 }],
        })
        .unwrap();
    assert_eq!(deleted.get_i32("n").unwrap(), 1, "delete removed nothing");

    let people = client.database("crud").collection::<Document>("people");
    let all = people.find(doc! {}).unwrap().try_collect().unwrap();
    assert_eq!(all.len(), 3);
    let pipeline_updated = people.find_one(doc! { "_id": 3 }).unwrap().unwrap();
    assert_eq!(
        pipeline_updated.get_i32("score").unwrap(),
        131,
        "aggregation-pipeline update did not apply"
    );
}

/// The matcher: comparison, logical, element, array and evaluation operators, plus the
/// find options that shape a result set.
fn query_operators(client: &Client) {
    let db = client.database("query");
    db.run_command(&doc! {
        "insert": "items",
        "documents": [
            { "_id": 1, "name": "alpha", "score": 1, "tags": ["red", "blue"], "meta": { "ok": true } },
            { "_id": 2, "name": "beta", "score": 5, "tags": ["blue"], "meta": { "ok": false } },
            { "_id": 3, "name": "gamma", "score": 9, "tags": ["green", "red"] },
            { "_id": 4, "name": "Delta", "score": 12 },
        ],
    })
    .unwrap();
    let items = db.collection::<Document>("items");

    let count = |filter: Document| items.find(filter).unwrap().try_collect().unwrap().len();

    assert_eq!(
        count(doc! { "score": { "$gt": 4, "$lte": 9 } }),
        2,
        "$gt/$lte"
    );
    assert_eq!(count(doc! { "score": { "$in": [1, 12] } }), 2, "$in");
    assert_eq!(count(doc! { "score": { "$nin": [1, 12] } }), 2, "$nin");
    assert_eq!(
        count(doc! { "$or": [{ "score": 1 }, { "score": 12 }] }),
        2,
        "$or"
    );
    assert_eq!(
        count(doc! { "$and": [{ "score": { "$gt": 1 } }, { "tags": "blue" }] }),
        1,
        "$and"
    );
    assert_eq!(count(doc! { "score": { "$not": { "$lt": 9 } } }), 2, "$not");
    assert_eq!(count(doc! { "tags": { "$exists": true } }), 3, "$exists");
    assert_eq!(count(doc! { "tags": { "$size": 2 } }), 2, "$size");
    assert_eq!(
        count(doc! { "tags": { "$all": ["red", "blue"] } }),
        1,
        "$all"
    );
    assert_eq!(
        count(doc! { "tags": { "$elemMatch": { "$eq": "green" } } }),
        1,
        "$elemMatch"
    );
    assert_eq!(count(doc! { "meta.ok": true }), 1, "dotted path");
    assert_eq!(count(doc! { "name": { "$type": "string" } }), 4, "$type");
    // $regex is PCRE2 inside the engine.
    assert_eq!(
        count(doc! { "name": { "$regex": "^a", "$options": "i" } }),
        1,
        "$regex"
    );
    assert_eq!(
        count(doc! { "$expr": { "$gt": ["$score", 8] } }),
        2,
        "$expr"
    );
    assert_eq!(count(doc! { "score": { "$mod": [2, 1] } }), 3, "$mod");

    // Sort, skip, limit and projection travel on the find command rather than the helper.
    let page = db
        .run_command(&doc! {
            "find": "items",
            "filter": {},
            "sort": { "score": -1 },
            "skip": 1,
            "limit": 2,
            "projection": { "name": 1, "_id": 0 },
        })
        .unwrap();
    let batch = page
        .get_document("cursor")
        .unwrap()
        .get_array("firstBatch")
        .unwrap();
    assert_eq!(batch.len(), 2, "limit ignored");
    let first = batch[0].as_document().unwrap();
    assert_eq!(first.get_str("name").unwrap(), "gamma", "sort/skip wrong");
    assert!(!first.contains_key("_id"), "projection kept _id");

    let distinct = db
        .run_command(&doc! { "distinct": "items", "key": "tags" })
        .unwrap();
    assert_eq!(distinct.get_array("values").unwrap().len(), 3, "distinct");

    let counted = db
        .run_command(&doc! { "count": "items", "query": { "score": { "$gte": 5 } } })
        .unwrap();
    assert_eq!(counted.get_i32("n").unwrap(), 3, "count");
}

/// Index types. Several of these pull in their own subsystem — `text` needs the full-text
/// stack and its stemmer, `2dsphere` needs the S2 geometry library.
fn indexes(client: &Client) {
    let db = client.database("indexes");
    db.run_command(&doc! {
        "insert": "docs",
        "documents": [
            { "_id": 1, "email": "a@example.com", "kind": "x", "when": "2026-01-01T00:00:00Z",
              "body": "the quick brown foxes jumped", "where": { "type": "Point", "coordinates": [1.0, 2.0] } },
            { "_id": 2, "email": "b@example.com", "kind": "y",
              "body": "lazy dogs sleeping", "where": { "type": "Point", "coordinates": [3.0, 4.0] } },
        ],
    })
    .unwrap();

    db.run_command(&doc! {
        "createIndexes": "docs",
        "indexes": [
            { "key": { "email": 1 }, "name": "email_unique", "unique": true },
            { "key": { "kind": 1, "_id": -1 }, "name": "compound" },
            { "key": { "kind": 1 }, "name": "partial",
              "partialFilterExpression": { "kind": { "$eq": "x" } } },
            { "key": { "when": 1 }, "name": "ttl", "expireAfterSeconds": 86400 },
            { "key": { "body": "text" }, "name": "text" },
            { "key": { "where": "2dsphere" }, "name": "geo" },
            { "key": { "kind": "hashed" }, "name": "hashed" },
            { "key": { "extra.$**": 1 }, "name": "wildcard" },
        ],
    })
    .unwrap();

    let listed = db.run_command(&doc! { "listIndexes": "docs" }).unwrap();
    let names: Vec<_> = listed
        .get_document("cursor")
        .unwrap()
        .get_array("firstBatch")
        .unwrap()
        .iter()
        .map(|index| {
            index
                .as_document()
                .unwrap()
                .get_str("name")
                .unwrap()
                .to_owned()
        })
        .collect();
    for expected in [
        "email_unique",
        "compound",
        "partial",
        "ttl",
        "text",
        "geo",
        "hashed",
        "wildcard",
    ] {
        assert!(
            names.contains(&expected.to_owned()),
            "index {expected} missing"
        );
    }

    // The unique index has to actually reject a duplicate, not merely exist.
    let duplicate = db.run_command(&doc! {
        "insert": "docs",
        "documents": [{ "_id": 3, "email": "a@example.com" }],
    });
    assert!(
        duplicate.is_err(),
        "unique index did not reject a duplicate"
    );

    // Text search and geo queries exercise the index implementations, not just their catalogs.
    let found = db
        .run_command(&doc! { "find": "docs", "filter": { "$text": { "$search": "foxes" } } })
        .unwrap();
    assert_eq!(
        found
            .get_document("cursor")
            .unwrap()
            .get_array("firstBatch")
            .unwrap()
            .len(),
        1,
        "$text search returned the wrong number of documents"
    );

    let near = db
        .run_command(&doc! {
            "find": "docs",
            "filter": { "where": { "$near": {
                "$geometry": { "type": "Point", "coordinates": [1.0, 2.0] },
                "$maxDistance": 100000,
            } } },
        })
        .unwrap();
    assert!(
        !near
            .get_document("cursor")
            .unwrap()
            .get_array("firstBatch")
            .unwrap()
            .is_empty(),
        "$near returned nothing"
    );

    db.run_command(&doc! { "dropIndexes": "docs", "index": "compound" })
        .unwrap();
}

/// The planner has to choose the index, not just have one available -- this notices if query
/// planning silently degrades to collection scans. It also covers `explain` itself, which
/// reports server version information and so aborted the process until `native/BUILD.bazel`
/// picked up `//src/mongo/util:version_impl`.
fn index_is_actually_used(client: &Client) {
    let db = client.database("indexes");
    let explained = db
        .run_command(&doc! {
            "explain": { "find": "docs", "filter": { "email": "a@example.com" } },
            "verbosity": "queryPlanner",
        })
        .unwrap();
    let plan = format!("{:?}", explained.get_document("queryPlanner").unwrap());
    assert!(
        plan.contains("IXSCAN"),
        "planner chose a collection scan over the unique index: {plan}"
    );
}

/// ICU collation. `patches/0001` keeps the root tables and the locales that alias onto them.
fn collation(client: &Client) {
    let db = client.database("collation");
    db.run_command(&doc! {
        "create": "words",
        "collation": { "locale": "en", "strength": 2 },
    })
    .unwrap();
    db.run_command(&doc! {
        "insert": "words",
        "documents": [{ "_id": 1, "word": "Ada" }],
    })
    .unwrap();

    let words = db.collection::<Document>("words");
    assert!(
        words.find_one(doc! { "word": "ada" }).unwrap().is_some(),
        "collection-default collation is not case-insensitive at strength 2"
    );

    // A per-operation collation is a different path from the collection default.
    let sorted = db
        .run_command(&doc! {
            "find": "words",
            "filter": { "word": "ADA" },
            "collation": { "locale": "en", "strength": 2 },
        })
        .unwrap();
    assert_eq!(
        sorted
            .get_document("cursor")
            .unwrap()
            .get_array("firstBatch")
            .unwrap()
            .len(),
        1,
        "per-operation collation ignored"
    );

    // A locale whose tables the patch dropped must fail cleanly rather than crash.
    assert!(
        db.run_command(&doc! {
            "create": "chinese",
            "collation": { "locale": "zh" },
        })
        .is_err(),
        "a dropped locale should be rejected"
    );
}

/// The aggregation pipeline: the stages an embedded store is actually used for.
fn aggregation_stages(client: &Client) {
    let db = client.database("agg");
    db.run_command(&doc! {
        "insert": "sales",
        "documents": [
            { "_id": 1, "product": "keyboard", "region": "eu", "units": 3, "price": 100, "parent": null },
            { "_id": 2, "product": "mouse", "region": "eu", "units": 7, "price": 25, "parent": "keyboard" },
            { "_id": 3, "product": "monitor", "region": "us", "units": 1, "price": 250, "parent": null },
            { "_id": 4, "product": "cable", "region": "us", "units": 11, "price": 5, "parent": "monitor" },
        ],
    })
    .unwrap();
    db.run_command(&doc! {
        "insert": "regions",
        "documents": [
            { "_id": "eu", "label": "Europe" },
            { "_id": "us", "label": "United States" },
        ],
    })
    .unwrap();

    let sales = db.collection::<Document>("sales");

    // $group with the common accumulators, then $sort/$limit on the grouped output.
    let grouped = sales
        .aggregate([
            doc! { "$match": { "units": { "$gt": 0 } } },
            doc! { "$group": {
                "_id": "$region",
                "revenue": { "$sum": { "$multiply": ["$units", "$price"] } },
                "average": { "$avg": "$price" },
                "cheapest": { "$min": "$price" },
                "dearest": { "$max": "$price" },
                "products": { "$push": "$product" },
                "distinct": { "$addToSet": "$region" },
                "spread": { "$stdDevPop": "$price" },
                "total": { "$count": {} },
            } },
            doc! { "$sort": { "revenue": -1 } },
        ])
        .unwrap()
        .try_collect()
        .unwrap();
    assert_eq!(
        grouped.len(),
        2,
        "$group produced the wrong number of buckets"
    );
    assert_eq!(grouped[0].get_str("_id").unwrap(), "eu");
    assert_eq!(grouped[0].get_i32("revenue").unwrap(), 475);
    assert_eq!(grouped[0].get_array("products").unwrap().len(), 2, "$push");

    // $lookup joins across collections; $unwind flattens the result.
    let joined = sales
        .aggregate([
            doc! { "$lookup": {
                "from": "regions",
                "localField": "region",
                "foreignField": "_id",
                "as": "region_doc",
            } },
            doc! { "$unwind": "$region_doc" },
            doc! { "$match": { "region_doc.label": "Europe" } },
        ])
        .unwrap()
        .try_collect()
        .unwrap();
    assert_eq!(joined.len(), 2, "$lookup + $unwind");

    // $graphLookup walks a self-referencing edge.
    let walked = sales
        .aggregate([
            doc! { "$match": { "product": "keyboard" } },
            doc! { "$graphLookup": {
                "from": "sales",
                "startWith": "$product",
                "connectFromField": "product",
                "connectToField": "parent",
                "as": "children",
            } },
        ])
        .unwrap()
        .try_collect()
        .unwrap();
    assert_eq!(
        walked[0].get_array("children").unwrap().len(),
        1,
        "$graphLookup"
    );

    // $facet runs several sub-pipelines over one input stream.
    let faceted = sales
        .aggregate([doc! { "$facet": {
            "by_region": [{ "$sortByCount": "$region" }],
            "bucketed": [{ "$bucket": {
                "groupBy": "$price",
                "boundaries": [0, 50, 500],
                "default": "other",
                "output": { "n": { "$sum": 1 } },
            } }],
            "top": [{ "$sort": { "price": -1 } }, { "$limit": 1 }, { "$project": { "product": 1, "_id": 0 } }],
        } }])
        .unwrap()
        .try_collect()
        .unwrap();
    let facet = &faceted[0];
    assert_eq!(
        facet.get_array("by_region").unwrap().len(),
        2,
        "$sortByCount"
    );
    assert_eq!(facet.get_array("bucketed").unwrap().len(), 2, "$bucket");
    assert_eq!(
        facet.get_array("top").unwrap().len(),
        1,
        "$facet sub-pipeline"
    );

    // Reshaping stages.
    let reshaped = sales
        .aggregate([
            doc! { "$addFields": { "value": { "$multiply": ["$units", "$price"] } } },
            doc! { "$replaceRoot": { "newRoot": { "product": "$product", "value": "$value" } } },
            doc! { "$sort": { "value": -1 } },
            doc! { "$skip": 1 },
            doc! { "$limit": 2 },
        ])
        .unwrap()
        .try_collect()
        .unwrap();
    assert_eq!(reshaped.len(), 2, "$replaceRoot/$skip/$limit");
    assert!(reshaped[0].contains_key("value"), "$addFields");

    // Sorting a large input with a small memory limit forces the spill-to-disk path.
    let many: Vec<Bson> = (0..2000)
        .map(|index| Bson::Document(doc! { "index": index, "pad": "x".repeat(200) }))
        .collect();
    db.run_command(&doc! { "insert": "wide", "documents": many })
        .unwrap();
    let spilled = db
        .run_command(&doc! {
            "aggregate": "wide",
            "pipeline": [{ "$sort": { "pad": 1, "index": -1 } }, { "$limit": 3 }],
            "allowDiskUse": true,
            "cursor": {},
        })
        .unwrap();
    assert_eq!(
        spilled
            .get_document("cursor")
            .unwrap()
            .get_array("firstBatch")
            .unwrap()
            .len(),
        3,
        "sort with allowDiskUse"
    );
}

/// Aggregation expressions, which are a separate evaluator from the stages.
fn aggregation_expressions(client: &Client) {
    let db = client.database("agg");
    let sales = db.collection::<Document>("sales");

    let computed = sales
        .aggregate([
            doc! { "$match": { "_id": 1 } },
            doc! { "$project": {
                "_id": 0,
                "upper": { "$toUpper": "$product" },
                "sliced": { "$substrCP": ["$product", 0, 3] },
                "concatenated": { "$concat": ["$region", "-", "$product"] },
                "conditional": { "$cond": [{ "$gt": ["$units", 1] }, "many", "few"] },
                "switched": { "$switch": {
                    "branches": [{ "case": { "$eq": ["$region", "eu"] }, "then": "europe" }],
                    "default": "elsewhere",
                } },
                "bound": { "$let": { "vars": { "double": { "$multiply": ["$units", 2] } },
                                     "in": { "$add": ["$$double", 1] } } },
                "mapped": { "$map": { "input": [1, 2, 3], "as": "n", "in": { "$multiply": ["$$n", 10] } } },
                "filtered": { "$filter": { "input": [1, 2, 3, 4], "as": "n",
                                           "cond": { "$gt": ["$$n", 2] } } },
                "reduced": { "$reduce": { "input": [1, 2, 3], "initialValue": 0,
                                          "in": { "$add": ["$$value", "$$this"] } } },
                "matched": { "$regexMatch": { "input": "$product", "regex": "^key" } },
                "stamped": { "$dateToString": { "date": "$$NOW", "format": "%Y" } },
                "coalesced": { "$ifNull": ["$missing", "fallback"] },
                "size": { "$size": [[1, 2]] },
                "rounded": { "$round": [{ "$divide": ["$price", 3] }, 2] },
            } },
        ])
        .unwrap()
        .try_collect()
        .unwrap();
    let row = &computed[0];
    assert_eq!(row.get_str("upper").unwrap(), "KEYBOARD", "$toUpper");
    assert_eq!(row.get_str("sliced").unwrap(), "key", "$substrCP");
    assert_eq!(
        row.get_str("concatenated").unwrap(),
        "eu-keyboard",
        "$concat"
    );
    assert_eq!(row.get_str("conditional").unwrap(), "many", "$cond");
    assert_eq!(row.get_str("switched").unwrap(), "europe", "$switch");
    assert_eq!(row.get_i32("bound").unwrap(), 7, "$let");
    assert_eq!(row.get_array("mapped").unwrap().len(), 3, "$map");
    assert_eq!(row.get_array("filtered").unwrap().len(), 2, "$filter");
    assert_eq!(row.get_i32("reduced").unwrap(), 6, "$reduce");
    assert!(row.get_bool("matched").unwrap(), "$regexMatch");
    assert_eq!(row.get_str("stamped").unwrap().len(), 4, "$dateToString");
    assert_eq!(row.get_str("coalesced").unwrap(), "fallback", "$ifNull");
    assert_eq!(row.get_i32("size").unwrap(), 2, "$size");
    assert!(row.get_f64("rounded").is_ok(), "$round");
}

/// `$out` and `$merge` write back into the storage engine from inside a pipeline.
fn aggregation_output_stages(client: &Client) {
    let db = client.database("agg");
    let sales = db.collection::<Document>("sales");

    sales
        .aggregate([
            doc! { "$group": { "_id": "$region", "revenue": { "$sum": "$price" } } },
            doc! { "$out": "by_region" },
        ])
        .unwrap()
        .try_collect()
        .unwrap();
    let out = db
        .collection::<Document>("by_region")
        .find(doc! {})
        .unwrap()
        .try_collect()
        .unwrap();
    assert_eq!(out.len(), 2, "$out did not materialize the collection");

    sales
        .aggregate([
            doc! { "$group": { "_id": "$region", "units": { "$sum": "$units" } } },
            doc! { "$merge": { "into": "by_region", "whenMatched": "merge", "whenNotMatched": "insert" } },
        ])
        .unwrap()
        .try_collect()
        .unwrap();
    let merged = db
        .collection::<Document>("by_region")
        .find_one(doc! { "_id": "eu" })
        .unwrap()
        .unwrap();
    assert!(
        merged.contains_key("units"),
        "$merge did not merge the field"
    );
    assert!(
        merged.contains_key("revenue"),
        "$merge dropped the existing field"
    );
}

/// Every BSON type has to survive a round trip through the storage engine, including
/// Decimal128, whose 2 MB of Intel library is deliberately still linked in.
fn bson_types(client: &Client) {
    use embedded_mongodb::bson::{
        Binary, DateTime, Decimal128, JavaScriptCodeWithScope, Regex, Timestamp, cstr,
        spec::BinarySubtype,
    };
    use std::str::FromStr;

    let db = client.database("types");
    let original = doc! {
        "_id": ObjectId::new(),
        "double": 1.5_f64,
        "string": "text",
        "document": { "nested": true },
        "array": [1, "two", 3.0],
        "binary": Binary { subtype: BinarySubtype::Generic, bytes: vec![1, 2, 3] },
        "uuid": Binary { subtype: BinarySubtype::Uuid, bytes: vec![7; 16] },
        "boolean": true,
        "date": DateTime::from_millis(1_700_000_000_000),
        "null": Bson::Null,
        "regex": Regex {
            pattern: cstr!("^a").into(),
            options: cstr!("i").into(),
        },
        "code": Bson::JavaScriptCode("return 1".to_owned()),
        "code_with_scope": JavaScriptCodeWithScope {
            code: "return x".to_owned(),
            scope: doc! { "x": 1 },
        },
        "int32": 7_i32,
        "int64": 9_223_372_036_854_775_807_i64,
        "timestamp": Timestamp { time: 42, increment: 1 },
        "decimal": Decimal128::from_str("1234.5678").unwrap(),
        "min": Bson::MinKey,
        "max": Bson::MaxKey,
    };
    let collection = db.collection::<Document>("everything");
    collection.insert_one(&original).unwrap();

    let stored = collection
        .find_one(doc! { "_id": original.get_object_id("_id").unwrap() })
        .unwrap()
        .unwrap();
    assert_eq!(stored, original, "a BSON type did not round trip");

    // Decimal128 has to be comparable and arithmetic-capable, not merely storable.
    let summed = collection
        .aggregate([doc! { "$group": {
            "_id": Bson::Null,
            "total": { "$sum": { "$add": ["$decimal", Decimal128::from_str("0.4322").unwrap()] } },
        } }])
        .unwrap()
        .try_collect()
        .unwrap();
    assert_eq!(
        summed[0].get("total").unwrap(),
        &Bson::Decimal128(Decimal128::from_str("1235.0000").unwrap()),
        "Decimal128 arithmetic"
    );

    assert!(
        collection
            .find_one(doc! { "decimal": { "$gt": Decimal128::from_str("1000").unwrap() } })
            .unwrap()
            .is_some(),
        "Decimal128 comparison in the matcher"
    );
}

/// Collection and database administration: the commands a host application needs to manage
/// storage, several of which live in code paths that also serve replication and sharding.
fn collection_administration(client: &Client) {
    let db = client.database("admin_surface");

    db.run_command(&doc! { "create": "capped", "capped": true, "size": 4096, "max": 2 })
        .unwrap();
    db.run_command(&doc! {
        "insert": "capped",
        "documents": [{ "n": 1 }, { "n": 2 }, { "n": 3 }],
    })
    .unwrap();
    let capped = db
        .collection::<Document>("capped")
        .find(doc! {})
        .unwrap()
        .try_collect()
        .unwrap();
    assert_eq!(capped.len(), 2, "capped collection did not evict");

    db.run_command(&doc! { "create": "view_source" }).unwrap();
    db.run_command(&doc! {
        "insert": "view_source",
        "documents": [{ "keep": true }, { "keep": false }],
    })
    .unwrap();
    db.run_command(&doc! {
        "create": "a_view",
        "viewOn": "view_source",
        "pipeline": [{ "$match": { "keep": true } }],
    })
    .unwrap();
    let through_view = db
        .collection::<Document>("a_view")
        .find(doc! {})
        .unwrap()
        .try_collect()
        .unwrap();
    assert_eq!(through_view.len(), 1, "reading through a view");

    let collections = db.run_command(&doc! { "listCollections": 1 }).unwrap();
    assert!(
        !collections
            .get_document("cursor")
            .unwrap()
            .get_array("firstBatch")
            .unwrap()
            .is_empty(),
        "listCollections returned nothing"
    );

    let databases = client
        .run_command("admin", &doc! { "listDatabases": 1 })
        .unwrap();
    assert!(
        !databases.get_array("databases").unwrap().is_empty(),
        "listDatabases returned nothing"
    );

    assert!(
        db.run_command(&doc! { "collStats": "capped" }).is_ok(),
        "collStats"
    );
    assert!(db.run_command(&doc! { "dbStats": 1 }).is_ok(), "dbStats");
    assert!(
        client
            .run_command("admin", &doc! { "serverStatus": 1 })
            .is_ok(),
        "serverStatus"
    );
    assert!(
        client.run_command("admin", &doc! { "isMaster": 1 }).is_ok(),
        "isMaster"
    );
    // The driver handshake. Recording client metadata logs the client's remote address, which
    // an in-process client does not have; until patches/0002 that aborted the process on the
    // very first hello. Run it twice -- metadata is only recorded on the first one.
    assert!(
        client.run_command("admin", &doc! { "hello": 1 }).is_ok(),
        "hello"
    );
    assert!(
        client.run_command("admin", &doc! { "hello": 1 }).is_ok(),
        "second hello"
    );
    assert!(
        client
            .run_command("admin", &doc! { "buildInfo": 1 })
            .is_ok(),
        "buildInfo"
    );

    let validated = db.run_command(&doc! { "validate": "view_source" }).unwrap();
    assert!(
        validated.get_bool("valid").unwrap(),
        "validate reported corruption"
    );

    client
        .run_command(
            "admin",
            &doc! {
                "renameCollection": "admin_surface.view_source",
                "to": "admin_surface.renamed",
            },
        )
        .unwrap();
    assert!(
        db.run_command(&doc! { "count": "renamed" }).is_ok(),
        "renameCollection"
    );

    // Schema validation is enforced by the write path.
    db.run_command(&doc! {
        "create": "validated",
        "validator": { "$jsonSchema": {
            "bsonType": "object",
            "required": ["n"],
            "properties": { "n": { "bsonType": "int" } },
        } },
    })
    .unwrap();
    assert!(
        db.run_command(&doc! { "insert": "validated", "documents": [{ "n": "not an int" }] })
            .is_err(),
        "$jsonSchema validator did not reject a bad document"
    );

    db.run_command(&doc! { "drop": "capped" }).unwrap();
}

/// Failures have to arrive as clean errors rather than as a crashed engine.
fn error_paths(client: &Client) {
    let db = client.database("errors");

    let unknown = client.run_command("admin", &doc! { "notACommand": 1 });
    assert!(unknown.is_err(), "unknown command was accepted");

    let bad_operator = db.run_command(&doc! {
        "find": "anything",
        "filter": { "x": { "$nope": 1 } },
    });
    assert!(bad_operator.is_err(), "unknown query operator was accepted");

    let bad_pipeline = db.run_command(&doc! {
        "aggregate": "anything",
        "pipeline": [{ "$notAStage": {} }],
        "cursor": {},
    });
    assert!(
        bad_pipeline.is_err(),
        "unknown aggregation stage was accepted"
    );

    // Reading a collection that does not exist is empty, not an error.
    let missing = db
        .collection::<Row>("never_created")
        .find(doc! {})
        .unwrap()
        .try_collect()
        .unwrap();
    assert!(missing.is_empty());

    // The engine keeps working after all of that.
    let rows = db.collection::<Row>("after_errors");
    rows.insert_one(Row {
        id: None,
        name: "still here".to_owned(),
        score: 1,
    })
    .unwrap();
    assert_eq!(rows.find(doc! {}).unwrap().try_collect().unwrap().len(), 1);
}

/// The three surfaces that identify this engine as the embedded build.
///
/// Nothing can attach a shell or Compass to an in-process engine, so this is how a caller
/// finds out what it is talking to. All three are registered from
/// `embedded-mongodb-sys/native/embedded_mongodb_native.cpp`, so they survive a submodule
/// bump; if one disappears, a build change took it away.
fn embedded_identity(client: &Client) {
    let build_info = client
        .run_command("admin", &doc! { "buildInfo": 1 })
        .expect("buildInfo failed");
    let modules: Vec<&str> = build_info
        .get_array("modules")
        .expect("buildInfo has no modules")
        .iter()
        .filter_map(Bson::as_str)
        .collect();
    assert!(
        modules.contains(&"embedded"),
        "buildInfo does not report the embedded module: {modules:?}"
    );
    // The extra fields land in buildEnvironment: that is where `inBuildInfo` entries go.
    let environment = build_info
        .get_document("buildEnvironment")
        .expect("buildInfo has no buildEnvironment");
    assert_eq!(
        environment.get_str("embedded").ok(),
        Some("true"),
        "buildEnvironment is missing the embedded field"
    );
    assert!(
        !environment
            .get_str("embeddedAuthor")
            .unwrap_or_default()
            .is_empty(),
        "buildEnvironment names no author"
    );
    // The engine's own provenance must survive being decorated.
    assert!(
        build_info.get_str("gitVersion").is_ok(),
        "buildInfo lost gitVersion"
    );
    assert!(
        build_info.get_str("version").is_ok(),
        "buildInfo lost version"
    );

    let status = client
        .run_command("admin", &doc! { "serverStatus": 1 })
        .expect("serverStatus failed");
    let embedded = status
        .get_document("embedded")
        .expect("serverStatus has no embedded section");
    assert_eq!(embedded.get_bool("embedded").ok(), Some(true));
    assert!(
        !embedded.get_str("author").unwrap_or_default().is_empty(),
        "embedded section names no author"
    );

    let about = client
        .run_command("admin", &doc! { "embeddedMongodb": 1 })
        .expect("embeddedMongodb command failed");
    assert_eq!(about.get_bool("embedded").ok(), Some(true));
    assert!(
        about
            .get_str("repository")
            .unwrap_or_default()
            .starts_with("https://"),
        "embeddedMongodb reports no repository"
    );
    // Same payload from the command and the serverStatus section.
    assert_eq!(
        about.get_str("author").ok(),
        embedded.get_str("author").ok(),
        "command and serverStatus disagree about the author"
    );
}
