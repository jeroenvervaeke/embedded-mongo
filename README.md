# embedded-mongo

Linux feasibility spike for using MongoDB like SQLite from Rust:

```rust
use embedded_mongodb::{Client, bson::doc};

let client = Client::new("./data")?;
let items = client.database("app").collection("items");
let inserted = items.insert_one(doc! { "name": "embedded" })?;
let item = items.find_one(doc! { "_id": inserted.inserted_id })?;
assert_eq!(item.unwrap().get_str("name")?, "embedded");
client.close()?;
# Ok::<(), anyhow::Error>(())
```

This prototype does not start `mongod`, create a child process, open a listening socket, or use the
MongoDB wire protocol. The call path is:

```text
bson::Document
  -> Rust/CXX bridge
  -> DBDirectClient
  -> ServiceEntryPointShardRole
  -> MongoDB command/query/catalog code
  -> WiredTiger files in the supplied directory
```

## What works

- MongoDB is pinned as the shallow `mongo/` submodule.
- `Client::new(path)` starts WiredTiger inside the caller's process.
- `database(name)` and `collection::<T>(name)` return cheap handles without performing I/O.
- Collections support typed `insert_one`, `insert_many`, `find_one`, batched `find`, and aggregation
  pipelines.
- `run_command(database, document)` accepts and returns BSON documents.
- Missing `_id` fields receive an `ObjectId`, matching the official drivers.
- A `Client` can be shared across threads; its native `ClientStrand` safely serializes commands.
- The test covers typed operations, concurrent calls, cursor `getMore`, command errors, and
  persistence across close/reopen.
- Startup is bounded to a 256 MB WiredTiger cache plus a 64 MB spill cache.
- One runtime may be active per process; sequential close/reopen is supported.

The `embedded-mongodb-sys` crate owns the native implementation, CXX bridge, and build script. It
exposes one raw command operation; the safe Rust crate builds BSON helpers on top.

## Examples

[`examples/basic.rs`](examples/basic.rs) opens a temporary directory and inserts one document:

```sh
cargo run --release --example basic
```

[`examples/advanced.rs`](examples/advanced.rs) chains typed batch inserts, a filtered cursor, and an
insert followed by `find_one`:

```sh
cargo run --release --example advanced
```

[`examples/aggregation.rs`](examples/aggregation.rs) builds a product sales report from nested
orders with a multi-stage aggregation pipeline:

```sh
cargo run --release --example aggregation
```

## Benchmark

Criterion measures open, `insert_one`, `find_one`, and close separately:

```sh
cargo bench --bench operations
```

## Build and test

MongoDB's pinned build requires Python 3.13, Bazel, C++20, lld, and a supported compiler. Its build
documentation currently lists GCC 14.2 or Clang 19.1 and roughly 13 GB of free space.

```sh
git submodule update --init --depth 1

cd mongo
python3.13 buildscripts/install_bazel.py
export PATH="$HOME/.local/bin:$PATH"
cd ..
cargo test
```

`embedded-mongodb-sys/build.rs` runs the pinned Bazel target and rebuilds it incrementally when
the native sources change. Set `BAZEL` to use a Bazel executable outside `PATH`, or set
`EMBEDDED_MONGODB_NATIVE_LIB_DIR` to skip Bazel and use a prebuilt library. Native compilation is
limited to eight parallel jobs by default; override it with `EMBEDDED_MONGODB_BAZEL_JOBS`.
Cargo-run tests and examples find it through the sys crate's build output; standalone binaries
still need the shared library in the platform loader path.

The current fast-build shared library is about 1.4 GB. Size optimization and packaging are not part
of this spike.

## Feasibility verdict

Yes, a current in-process SDK can be built, and the persistence test proves the central path.
However, this is not a supported MongoDB embedding mode.

MongoDB removed the complete embedded SDK in
[SERVER-70429](https://github.com/mongodb/mongo/commit/3573b2cc82f3f7483d54c35d8ca7267defc650d3)
on May 10, 2024. The remaining
[`stitch_support`](mongo/src/mongo/embedded/stitch_support/stitch_support.h) code only evaluates
match, projection, and update expressions; it does not own storage.

This spike reconstructs the removed SDK's essential shape against current internals:

- one process-global service context and storage lifecycle;
- in-memory command dispatch through
  [`DBDirectClient`](mongo/src/mongo/db/dbdirectclient.h);
- current command, query, catalog, index, and WiredTiger classes;
- a small stable C ABI hidden behind CXX and Rust BSON types.

It does not call `mongod_main`. That path owns process signals, global shutdown, transport, and
fatal process behavior that are unsuitable for a library.

## Hard constraints

- MongoDB server internals are private and change frequently. Each submodule update can require
  lifecycle and Bazel dependency work.
- Many server components assume one global runtime. Multiple simultaneous `Client` values or
  different active database directories are rejected.
- Commands issued through one `Client` are thread-safe but serialized, not run in parallel.
- There is no process isolation: a MongoDB fatal invariant, memory fault, or abort terminates the
  Rust host.
- Only `insert`, `find`, aggregation, clean shutdown, and recovery are covered. Authentication,
  replication, transactions, change streams, TTL, backup, encryption, and the wider command set
  are not yet supported claims.
- The current bridge is Linux-specific (`.so` loading and ELF startup initialization).
- MongoDB server code is SSPL-1.0. Distribution or service use requires a license review; see
  [`LICENSE-Community.txt`](mongo/LICENSE-Community.txt).

This is a credible private, pinned SDK experiment. Turning it into a production crate means owning
a MongoDB fork, a narrow supported command matrix, crash testing, packaging, and upgrade work.
