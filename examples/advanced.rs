use anyhow::Result;
use embedded_mongodb::{
    Client,
    bson::{doc, oid::ObjectId},
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
struct Book {
    #[serde(rename = "_id", skip_serializing_if = "Option::is_none")]
    id: Option<ObjectId>,
    title: String,
    year: i32,
    tags: Vec<String>,
}

fn main() -> Result<()> {
    // The database files are deleted when this temporary directory is dropped.
    let data_directory = tempfile::tempdir()?;
    let client = Client::new(data_directory.path())?;
    let library = client.database("library");
    let books = library.collection::<Book>("books");

    // Insert several typed Rust values at once.
    books.insert_many([
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
    ])?;

    // Query with MongoDB operators and deserialize every match into a Book.
    let recent_rust_books = books
        .find(doc! {
            "year": { "$gte": 2020 },
            "tags": "rust",
        })?
        .try_collect()?;
    assert_eq!(recent_rust_books.len(), 1);
    println!("recent Rust books: {recent_rust_books:#?}");

    // Use MongoDB's generated _id to read an inserted book back.
    let result = books.insert_one(Book {
        id: None,
        title: "MongoDB Inside Rust".to_owned(),
        year: 2026,
        tags: vec!["database".to_owned(), "rust".to_owned()],
    })?;
    let inserted_book = books
        .find_one(doc! { "_id": result.inserted_id })?
        .expect("inserted book should be found");
    assert_eq!(inserted_book.title, "MongoDB Inside Rust");
    println!("inserted and fetched: {inserted_book:#?}");

    client.close()?;
    Ok(())
}
