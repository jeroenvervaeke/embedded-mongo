# Embedded MongoDB

**The MongoDB version of SQLite.**

The real MongoDB engine, embedded directly in your application.

One process. One local directory. MongoDB's own queries, aggregations, commands, and WiredTiger
storage.

No server deployment. No ports. No connection string.

> [!WARNING]
> **Experimental.** This project was created by
> [Jeroen Vervaeke](https://github.com/jeroenvervaeke). It is not production-ready or supported by
> MongoDB.

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

## Python

The `pymongo-embedded` package keeps PyMongo's API for remote servers and routes embedded URIs to
the in-process engine:

```py
from pymongo_embedded import MongoClient

remote = MongoClient("mongodb://localhost:27017/")
local = MongoClient("mongodb_embedded://./data")

local.app.items.insert_one({"name": "embedded"})
```

`mongodb+embedded://./data` is the URI-valid spelling and behaves identically.

Run the included [Python example](examples/python/basic.py) with:

```sh
./scripts/python
```

The runner creates a clean environment under `.cache/python`, builds incrementally, and uses the
sibling `mongo-python-driver` checkout. It also accepts normal Python arguments:

```sh
./scripts/python -i              # Open a Python shell.
./scripts/python your_script.py
./scripts/python -m pip install another-package
```

Set `PYMONGO_SOURCE` if the PyMongo checkout is elsewhere.

To build a distributable wheel containing the native engine:

```sh
python -m pip install "maturin[patchelf]"
maturin build --release
python -m pip install target/wheels/pymongo_embedded-*.whl
```

The first build compiles the pinned MongoDB submodule.

The initial binding supports synchronous PyMongo 4.18 commands, including normal CRUD, cursors,
aggregations, and bulk document sequences. Authentication, TLS, compression, sessions,
transactions, change streams, exhaust cursors, and async PyMongo are not supported.

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
- **Portability and upgrades** — automated `mongod` handoff, MongoDB upgrades, and Windows.

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

MongoDB's pinned build requires Python 3.13, Bazel, C++20, and a supported compiler and linker. Its
build documentation currently lists GCC 14.2 or Clang 19.1 and roughly 13 GB of free space.

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

- **Release:** about 45 MB, or 34 MB with the patches in `patches/` applied.
- **Debug:** about 1.4 GB.

The release build is size-optimized rather than speed-optimized: `-Os`, link-time optimization,
per-function and per-data sections with `--gc-sections`, packed relative relocations, only the
five `extern "C"` entry points exported, and no TLS, gRPC, OpenTelemetry or enterprise modules.
Run `./scripts/apply-mongo-patches` before building. It trims the embedded ICU collation
tables (2.6 MB), removes the slot-based execution engine so queries run on the classic one
(4.7 MB), the replication implementation the embedded server never uses (1.1 MB) and the
sharding runtime (2.5 MB), and fixes an assertion that aborted the host process on the first `hello` a driver
sends. See
[`docs/native-size-reduction.md`](docs/native-size-reduction.md) for the measurements and
what further reduction would cost. Packed relative relocations require glibc 2.36 or newer.
LTO is linked with `ld.bfd`, which must be on `PATH`; lld cannot read GCC's IR.

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
- Linux and macOS are tested; Windows is untested.
- This project embeds MongoDB Community Server as a modified work first published on 2026-07-27
  and licensed as a whole under SSPL-1.0. Distribution or service use requires a license review;
  see [`LICENSE`](LICENSE).

Production use would require owning a MongoDB fork, a narrow supported command matrix, crash
testing, and upgrade work.
