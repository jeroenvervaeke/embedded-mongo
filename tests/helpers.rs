use embedded_mongodb::{
    Client, Error,
    bson::{Bson, doc, oid::ObjectId},
};
use serde::{Deserialize, Serialize};
use std::thread;

#[derive(Debug, Deserialize, Serialize)]
struct Item {
    #[serde(rename = "_id", default, skip_serializing_if = "Option::is_none")]
    id: Option<ObjectId>,
    name: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct Book {
    #[serde(rename = "_id", skip_serializing_if = "Option::is_none")]
    id: Option<ObjectId>,
    title: String,
    year: i32,
    tags: Vec<String>,
}

#[test]
fn features_work_end_to_end() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Client>();

    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("database");

    let client = Client::new(&path).unwrap();
    let ping = client
        .database("admin")
        .run_command(&doc! { "ping": 1 })
        .unwrap();
    assert_eq!(ping.get_f64("ok").unwrap(), 1.0);

    let items = client.database("test").collection::<Item>("items");
    let inserted = items
        .insert_one(Item {
            id: None,
            name: "persisted".to_owned(),
        })
        .unwrap();
    let persisted_id = match inserted.inserted_id {
        Bson::ObjectId(id) => id,
        id => panic!("expected ObjectId, got {id:?}"),
    };

    let inserted = items
        .insert_many((0..110).map(|index| Item {
            id: None,
            name: format!("batch-{index}"),
        }))
        .unwrap();
    assert_eq!(inserted.inserted_ids.len(), 110);

    let books = client.database("library").collection::<Book>("books");
    books
        .insert_many([
            Book {
                id: None,
                title: "Rust Foundations".to_owned(),
                year: 2019,
                tags: vec!["rust".to_owned()],
            },
            Book {
                id: None,
                title: "Embedded Databases".to_owned(),
                year: 2024,
                tags: vec!["database".to_owned(), "rust".to_owned()],
            },
            Book {
                id: None,
                title: "Storage Engines".to_owned(),
                year: 2025,
                tags: vec!["database".to_owned()],
            },
        ])
        .unwrap();

    let recent_rust_books = books
        .find(doc! {
            "year": { "$gte": 2020 },
            "tags": "rust",
        })
        .unwrap()
        .try_collect()
        .unwrap();
    assert_eq!(recent_rust_books.len(), 1);

    let inserted = books
        .insert_one(Book {
            id: None,
            title: "MongoDB Inside Rust".to_owned(),
            year: 2026,
            tags: vec!["database".to_owned(), "rust".to_owned()],
        })
        .unwrap();
    let inserted_book = books
        .find_one(doc! { "_id": inserted.inserted_id })
        .unwrap()
        .unwrap();
    assert_eq!(inserted_book.title, "MongoDB Inside Rust");

    let orders = client.database("shop").collection("orders");
    orders
        .insert_many([
            doc! {
                "customer": "Ada",
                "status": "paid",
                "items": [
                    { "product": "Keyboard", "quantity": 1, "unit_price": 100 },
                    { "product": "Mouse", "quantity": 2, "unit_price": 25 },
                ],
            },
            doc! {
                "customer": "Grace",
                "status": "pending",
                "items": [
                    { "product": "Monitor", "quantity": 1, "unit_price": 250 },
                ],
            },
            doc! {
                "customer": "Linus",
                "status": "paid",
                "items": [
                    { "product": "Keyboard", "quantity": 2, "unit_price": 90 },
                    { "product": "Mouse", "quantity": 1, "unit_price": 25 },
                ],
            },
        ])
        .unwrap();

    let sales_report = orders
        .aggregate([
            doc! { "$match": { "status": "paid" } },
            doc! { "$unwind": "$items" },
            doc! {
                "$group": {
                    "_id": "$items.product",
                    "units_sold": { "$sum": "$items.quantity" },
                    "revenue": {
                        "$sum": {
                            "$multiply": ["$items.quantity", "$items.unit_price"],
                        },
                    },
                },
            },
            doc! { "$sort": { "revenue": -1 } },
            doc! {
                "$project": {
                    "_id": 0,
                    "product": "$_id",
                    "units_sold": 1,
                    "revenue": 1,
                },
            },
        ])
        .unwrap()
        .try_collect()
        .unwrap();
    assert_eq!(sales_report.len(), 2);
    assert_eq!(sales_report[0].get_str("product").unwrap(), "Keyboard");

    // "en" collates through ICU's root tables, which the embedded data file keeps even after
    // patches/0001 drops the large per-language ones.
    let collated = client.database("collation");
    collated
        .run_command(&doc! {
            "create": "words",
            "collation": { "locale": "en", "strength": 2 },
        })
        .unwrap();
    let words = collated.collection::<Item>("words");
    words
        .insert_one(Item {
            id: None,
            name: "Ada".to_owned(),
        })
        .unwrap();
    assert!(
        words.find_one(doc! { "name": "ada" }).unwrap().is_some(),
        "collation at strength 2 should match case-insensitively"
    );

    thread::scope(|scope| {
        for index in 0..4 {
            let client = &client;
            scope.spawn(move || {
                client
                    .database("test")
                    .collection::<Item>("items")
                    .insert_one(Item {
                        id: None,
                        name: format!("thread-{index}"),
                    })
                    .unwrap();
            });
        }
    });

    let error = items
        .insert_one(Item {
            id: Some(persisted_id),
            name: "duplicate".to_owned(),
        })
        .unwrap_err();
    assert!(matches!(&error, Error::Server { .. }));
    assert!(error.to_string().starts_with("MongoDB error 11000:"));

    let documents = items.find(doc! {}).unwrap().try_collect().unwrap();
    assert_eq!(documents.len(), 115);
    client.close().unwrap();

    let client = Client::new(&path).unwrap();
    let item = client
        .database("test")
        .collection::<Item>("items")
        .find_one(doc! { "_id": persisted_id })
        .unwrap()
        .unwrap();
    assert_eq!(item.name, "persisted");
    client.close().unwrap();
}
