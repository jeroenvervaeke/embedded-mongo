# embedded-mongo

Linux feasibility spike for using MongoDB like SQLite from Rust:

```rust
use embedded_mongodb::{Client, bson::doc};

let client = Client::new("./data")?;
let reply = client.run_command(
    "app",
    &doc! {
        "insert": "items",
        "documents": [{"_id": 1, "name": "embedded"}],
    },
)?;
assert_eq!(reply.get_i32("n")?, 1);
client.close()?;
# Ok::<(), Box<dyn std::error::Error>>(())
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
- `run_command(database, document)` accepts and returns BSON documents.
- The test inserts a document, cleanly closes the engine, reopens the same directory, and finds the
  persisted document.
- Startup is bounded to a 256 MB WiredTiger cache plus a 64 MB spill cache.
- One runtime may be active per process; sequential close/reopen is supported.

The bridge intentionally exposes one raw command operation. Collection-specific Rust wrappers can
be added after the supported command surface is known.

## Example

[`examples/basic.rs`](examples/basic.rs) opens a directory, inserts a document, queries it, and
closes the engine:

```sh
cargo run --release --example basic -- ./example-data
```

## Build and test

MongoDB's pinned build requires Python 3.13, Bazel, C++20, lld, and a supported compiler. Its build
documentation currently lists GCC 14.2 or Clang 19.1 and roughly 13 GB of free space.

```sh
git submodule update --init --depth 1

cd mongo
python3.13 buildscripts/install_bazel.py
export PATH="$HOME/.local/bin:$PATH"

CC=gcc-14 CXX=g++-14 bazel build \
  @mongot_localdev//:libembedded_mongodb_native.so \
  --override_repository=mongot_localdev="$(cd .. && pwd)/native" \
  --config=native_toolchain \
  --compiler_type=gcc \
  --disable_warnings_as_errors=True \
  --copt=-include \
  --copt=sys/syscall.h \
  --copt=-fPIC

cd ..
cargo test
```

`build.rs` looks for
`mongo/bazel-bin/external/mongot_localdev/libembedded_mongodb_native.so`. Set
`EMBEDDED_MONGODB_NATIVE_LIB_DIR` when the library is copied elsewhere.

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
- There is no process isolation: a MongoDB fatal invariant, memory fault, or abort terminates the
  Rust host.
- Only `insert`, `find`, clean shutdown, and recovery are covered. Authentication, replication,
  transactions, change streams, TTL, backup, encryption, and the wider command set are not yet
  supported claims.
- The current bridge is Linux-specific (`.so`, ELF startup initialization, and rpath handling).
- MongoDB server code is SSPL-1.0. Distribution or service use requires a license review; see
  [`LICENSE-Community.txt`](mongo/LICENSE-Community.txt).

This is a credible private, pinned SDK experiment. Turning it into a production crate means owning
a MongoDB fork, a narrow supported command matrix, crash testing, packaging, and upgrade work.
