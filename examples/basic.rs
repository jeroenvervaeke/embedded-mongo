use anyhow::Result;
use embedded_mongodb::{Client, bson::doc};

fn main() -> Result<()> {
    // The database files are deleted when this temporary directory is dropped.
    let data_directory = tempfile::tempdir()?;
    let client = Client::new(data_directory.path())?;
    let database = client.database("demo");
    let items = database.collection("items");

    let result = items.insert_one(doc! { "name": "embedded" })?;
    println!("inserted document id: {}", result.inserted_id);

    client.close()?;
    Ok(())
}
