use embedded_mongodb::{
    Client,
    bson::{doc, oid::ObjectId},
};
use std::{env, error::Error, path::PathBuf, time::Instant};

fn main() -> Result<(), Box<dyn Error>> {
    let path = env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("./example-data"));
    let id = ObjectId::new();
    let started = Instant::now();
    let client = Client::new(&path)?;
    let open_elapsed = started.elapsed();

    let started = Instant::now();
    let insert = client.run_command(
        "demo",
        &doc! {
            "insert": "items",
            "documents": [{"_id": id, "name": "embedded"}],
        },
    )?;
    if insert.get_i32("n")? != 1 {
        return Err(format!("insert failed: {insert:?}").into());
    }
    let insert_elapsed = started.elapsed();

    let started = Instant::now();
    let find = client.run_command(
        "demo",
        &doc! {
            "find": "items",
            "filter": {"_id": id},
        },
    )?;
    let documents = find.get_document("cursor")?.get_array("firstBatch")?;
    let find_elapsed = started.elapsed();

    println!("database: {}", path.display());
    println!("documents: {documents:#?}");

    let started = Instant::now();
    client.close()?;
    let close_elapsed = started.elapsed();
    println!(
        "timings_ms: open={:.3} insert={:.3} find={:.3} close={:.3}",
        open_elapsed.as_secs_f64() * 1_000.0,
        insert_elapsed.as_secs_f64() * 1_000.0,
        find_elapsed.as_secs_f64() * 1_000.0,
        close_elapsed.as_secs_f64() * 1_000.0,
    );
    Ok(())
}
