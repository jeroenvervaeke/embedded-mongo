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

#[test]
fn helpers_are_typed_persistent_and_thread_safe() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Client>();

    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("database");

    let client = Client::new(&path).unwrap();
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
