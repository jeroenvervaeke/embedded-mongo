# Embedded MongoDB

**The MongoDB version of SQLite.**

The real MongoDB engine, embedded directly in your application.

One process. One local directory. MongoDB's own queries, aggregations, commands, and WiredTiger
storage.

No server deployment. No ports. No connection string.

> [!WARNING]
> **Proof of concept.** This project was created by
> [Jeroen Vervaeke](https://github.com/jeroenvervaeke). It is experimental, Linux-only, not
> production-ready, and not supported by MongoDB.

## Features

**Not a MongoDB-compatible reimplementation. MongoDB's actual server code runs inside your
process.**

- 🍃 **Real MongoDB execution** — queries, cursors, aggregation pipelines, commands, and storage
  run through MongoDB's own engine, backed by WiredTiger.
- 📦 **SQLite-like deployment** — open a local directory; no database server to install or manage.
- 🦀 **Rust-native today, multi-language tomorrow** — work with typed Serde collections or raw
  MongoDB documents.
  - 🐍 **Python** — a binding can wrap the exported C ABI without changing the database engine.
  - 🟨 **JavaScript / Node.js** — the same boundary can expose the API to the JavaScript ecosystem.
- 💾 **Persistent storage** — clean close and reopen cycles preserve data in the supplied directory.
- 🧵 **Thread-safe access** — share one client across threads while commands are safely serialized.
- 🆔 **Automatic IDs** — missing `_id` fields receive an `ObjectId`, matching the official drivers.

## Deployment model

![Traditional MongoDB uses a separate server process; Embedded MongoDB runs inside the application while keeping an exchangeable native MongoDB data directory](docs/deployment-model.svg)

The directory uses MongoDB's native on-disk format. After a clean shutdown, hand it between
`embedded-mongodb` and the matching pinned `mongod` build; only one process may own it at a time.

<details>
<summary><strong>Technical implementation</strong></summary>

Commands travel from `bson::Document` through the safe Rust API, CXX bridge, `DBDirectClient`,
`ServiceEntryPointShardRole`, MongoDB command/query/catalog code, and finally WiredTiger.

MongoDB is pinned as the shallow `mongo/` submodule. The `embedded-mongodb-sys` crate owns the
native implementation, CXX bridge, and build script; the safe Rust crate builds BSON helpers on
top. Startup uses a 256 MB WiredTiger cache plus a 64 MB spill cache.

</details>

## Quick start

Open a directory, insert a document, and query it back:

```rust
use embedded_mongodb::bson::doc;

let client = embedded_mongodb::Client::new("./data")?;
let items = client.database("app").collection("items");
let inserted = items.insert_one(doc! { "name": "embedded" })?;
let item = items.find_one(doc! { "_id": inserted.inserted_id })?;
println!("{item:?}");
```

### Full examples

Explore the complete runnable examples:

- [Basic: insert a document](examples/basic.rs)
- [Typed: model and query a collection](examples/advanced.rs)
- [Aggregation: build a sales report](examples/aggregation.rs)

## What this unlocks

The embedded deployment model creates a path toward:

- 🖥️ **Local-first applications** with MongoDB data stored beside the app.
- 🧰 **Self-contained developer tools and CLIs** without a database service to provision.
- ✈️ **Offline and edge workloads** that keep working without network access.
- 🧪 **Tests and demos** that start with the application instead of waiting for infrastructure.
- 🌍 **One engine across ecosystems**, with Rust today and potential Python and JavaScript bindings.

## Test coverage

### ✅ Tested

- **Lifecycle and persistence** — open, clean close, reopen, and persistence on disk.
- **Documents and typing** — BSON and Serde-backed values, generated `ObjectId`s, `insert_one`, and
  `insert_many`.
- **Queries and cursors** — filtered `find`, `find_one`, array matching, comparison operators, and
  batched cursor `getMore`.
- **Commands** — `ping` through the public BSON `run_command` API.
- **Aggregation** — a multi-stage product report using `$match`, `$unwind`, `$group`, arithmetic,
  `$sort`, and `$project`.
- **Concurrency and errors** — `Client: Send + Sync`, concurrent inserts, and structured
  duplicate-key errors.
- **Observability** — MongoDB-to-`tracing` severity mapping.

One end-to-end integration test covers the operations demonstrated by all three examples.

### ⚠️ Not tested

- **Wider command set** — commands beyond those exercised by the covered helpers and `ping`.
- **Cursor cancellation** — early cursor drop and its `killCursors` cleanup path.
- **Advanced MongoDB features** — authentication, replication, transactions, change streams, TTL,
  backup, and encryption.
- **Failure and scale** — crash recovery, stress tests, and large-data workloads.
- **Portability and upgrades** — automated `mongod` handoff, packaging, MongoDB upgrades, and
  non-Linux platforms.

## Observability

MongoDB logs are emitted through `tracing` under the `embedded_mongodb::mongo` target. Events carry
the MongoDB ID, component, context, severity, and lossless JSON record; open, command, and close
operations add spans. Without a subscriber the library remains silent. The basic example installs
`tracing-subscriber`.

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
cargo test --workspace
```

`embedded-mongodb-sys/build.rs` runs the pinned Bazel target and rebuilds it incrementally when
the native sources change. Set `BAZEL` to use a Bazel executable outside `PATH`, or set
`EMBEDDED_MONGODB_NATIVE_LIB_DIR` to skip Bazel and use a prebuilt library. Native compilation is
limited to eight parallel jobs by default; override it with `EMBEDDED_MONGODB_BAZEL_JOBS`.
Cargo-run tests and examples find it through the sys crate's build output; standalone binaries
still need the shared library in the platform loader path.

The current fast-build shared library is about 1.4 GB. Size optimization and packaging are not part
of this spike.

## Hard constraints

- MongoDB server internals are private and change frequently. Each submodule update can require
  lifecycle and Bazel dependency work.
- Many server components assume one global runtime. Multiple simultaneous `Client` values or
  different active database directories are rejected.
- Commands issued through one `Client` are thread-safe but serialized, not run in parallel.
- There is no process isolation: a MongoDB fatal invariant, memory fault, or abort terminates the
  Rust host.
- Authentication, replication, transactions, change streams, TTL, backup, encryption, and the
  wider command set are outside the current supported scope.
- The current bridge is Linux-specific (`.so` loading and ELF startup initialization).
- This project embeds MongoDB Community Server as a modified work first published on 2026-07-27
  and licensed as a whole under SSPL-1.0. Distribution or service use requires a license review;
  see [`LICENSE`](LICENSE).

Production use would require owning a MongoDB fork, a narrow supported command matrix, crash
testing, packaging, and upgrade work.
