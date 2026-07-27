use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use embedded_mongodb::{Client, bson::doc};
use std::hint::black_box;
use tempfile::TempDir;

fn open_client() -> (Client, TempDir) {
    let directory = tempfile::tempdir().unwrap();
    let client = Client::new(directory.path()).unwrap();
    (client, directory)
}

fn operations(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("operations");

    group.bench_function("open", |bencher| {
        bencher.iter_batched(
            || tempfile::tempdir().unwrap(),
            |directory| {
                let client = Client::new(directory.path()).unwrap();
                (client, directory)
            },
            BatchSize::PerIteration,
        );
    });

    let (client, directory) = open_client();
    {
        let items = client.database("benchmark").collection("items");
        group.bench_function("insert_one", |bencher| {
            bencher.iter(|| black_box(items.insert_one(doc! { "name": "embedded" }).unwrap()));
        });
    }
    client.close().unwrap();
    drop(directory);

    let (client, directory) = open_client();
    {
        let items = client.database("benchmark").collection("items");
        let inserted = items.insert_one(doc! { "name": "embedded" }).unwrap();
        let filter = doc! { "_id": inserted.inserted_id };
        group.bench_function("find_one", |bencher| {
            bencher.iter(|| black_box(items.find_one(filter.clone()).unwrap()));
        });
    }
    client.close().unwrap();
    drop(directory);

    group.bench_function("close", |bencher| {
        bencher.iter_batched(
            open_client,
            |(client, directory)| {
                client.close().unwrap();
                directory
            },
            BatchSize::PerIteration,
        );
    });

    group.finish();
}

criterion_group!(benches, operations);
criterion_main!(benches);
