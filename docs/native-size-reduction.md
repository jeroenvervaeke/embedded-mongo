# Shrinking `libembedded_mongodb_native.so`

Goal: ship the engine inside a Java library. Target 25 MB, hard ceiling 50 MB.

This branch takes the release library from **147.1 MB to 54.05 MB (−63%)** with no
functionality removed. This document records how, what was measured, and what is left.

All sizes are bytes as reported by `stat -c%s`, on `x86_64-linux`, gcc 16.2.1, lld 22.1.8,
glibc 2.44.

## Where the size went

| Step | Size | Δ |
| --- | --- | --- |
| Baseline (`--config=opt`, `--strip-all`, `-fvisibility=hidden`) | 147,116,136 | — |
| `-ffunction-sections -fdata-sections` + `--gc-sections --icf=all` | 101,593,920 | −31% |
| `-Wl,-z,pack-relative-relocs` (DT_RELR) | 96,509,736 | −5.1 MB |
| `--//bazel/config:opt=size` (`-Os` instead of `-O2`) | 61,956,680 | −34.6 MB |
| `ssl=False`, `build_otel=False`, `build_enterprise=False` | **54,050,888** | −7.9 MB |

Verified at both 96.5 MB and 54.05 MB with `cargo test --release --all-targets`: the
end-to-end test (CRUD, aggregation, cross-thread access, error paths) and all three
criterion benchmarks pass.

### Why per-function sections were the unlock

MongoDB marks nearly every `cc_library` `alwayslink`. In the baseline link,
**4376 of 5909 objects (545 MB of `.o`) were force-linked** outside `--start-lib`, so the
linker was not permitted to drop them. On top of that, objects were compiled with a single
monolithic `.text` per translation unit, so `--gc-sections` had nothing to cut at —
relinking the baseline objects with `--gc-sections --icf=all` alone bought only 3.6%.

`-ffunction-sections -fdata-sections` gives the linker section granularity inside those
force-linked objects. That one change is worth 31%.

### Notes on individual flags

- **`--icf=all`** folds identical functions even when their addresses are taken. `--icf=safe`
  is the conservative choice if function-pointer identity ever turns out to matter; measured
  cost of downgrading was not taken, but `--icf=all` alone was worth only ~1.3% pre-gc.
- **DT_RELR** needs glibc ≥ 2.36 at runtime. If older glibc must be supported, drop
  `-Wl,-z,pack-relative-relocs` and add back 5.1 MB.
- **`ssl=False`** removes the TLS stack and, with it, the whole gRPC/protobuf/abseil tree
  (MongoDB gates gRPC on `ssl` — there is no separate switch). It also drops the definition
  of `SSLPeerInfo::forSession`, which `db/client.h` and the authentication commands still
  reference. `embedded-mongodb-sys/native/BUILD.bazel` therefore pulls that one 33-line file
  (`@//src/mongo/util/net:ssl_peer_info.cpp`) into our own target. This is in our overridden
  repository, not a submodule edit.

## The floor: 35.7 MB

Measured, not estimated. Relink every object lazily (each wrapped in its own
`--start-lib`/`--end-lib`) so only code reachable from the `extern "C"` entry points
survives: **35,686,400 bytes**.

That is the hard floor for the current dependency set. It ignores initializer-driven
registration so the result would not actually run, but nothing below it is reachable by
compiler flags, dead-stripping, or LTO — only by removing engine features.

**25 MB is below this floor.** It cannot come from build configuration.

## Technique: measure a cut without rebuilding

Bazel writes the full link command to a params file. Editing it and relinking takes about
one second, versus 40 minutes for a rebuild. This is how every "what would removing X save"
number below was obtained.

```sh
E=~/.cache/bazel/_bazel_jeroen/<hash>/execroot/_main
P=$E/bazel-out/k8-opt/bin/external/mongot_localdev/libembedded_mongodb_native.so-2.params

# Drop objects matching a pattern, retarget the output, allow the resulting dangling refs.
grep -vE 'bin/src/mongo/db/s/|bin/src/mongo/s/' $P \
  | sed 's|^bazel-out/.*/libembedded_mongodb_native.so$|/tmp/probe.so|;
         s|^-Wl,-z,defs,--strip-all$|-Wl,--strip-all|' > /tmp/probe.params
printf -- '-Wl,--unresolved-symbols=ignore-all\n-Wl,--noinhibit-exec\n' >> /tmp/probe.params
(cd $E && g++ @/tmp/probe.params) && stat -c%s /tmp/probe.so
```

Add `-Wl,-Map=/tmp/link.map` to get per-object, per-section attribution. The map is ~116 MB;
aggregate it by bazel target (the `_objs/<target>/` path component) rather than reading it.

## What is left, and what each costs

Measured by link-line pruning from the 54.05 MB build:

| Cut | Result | Saves |
| --- | --- | --- |
| ICU collation data | 50,081,448 | −4.0 MB |
| Sharding (`db/s`, `mongo/s`, `global_catalog`) | 48,016,840 | −6.0 MB |
| Replication implementation | 51,337,640 | −2.7 MB |
| All three | 41,228,808 | −12.8 MB |

All three have been approved by the repository owner. **All three require patching the
MongoDB submodule's BUILD files** — they cannot be done from `native/BUILD.bazel` alone:

- **ICU** — `db/query` (`src/mongo/db/query/BUILD.bazel:486`) and `db/exec/sbe`
  (`src/mongo/db/exec/sbe/BUILD.bazel:455`) depend on `collator_factory_icu` directly. The
  3.37 MB is a single `.rodata` object, `src/mongo/util:icu_init` — the `icudt57l.dat` blob
  compiled to a byte array by a `render_template` rule. Two routes: sever the dep and accept
  that `collation: {locale: ...}` stops working, or regenerate the blob from a trimmed
  `.dat` containing only the locales you care about. The second keeps functionality and is
  probably the better trade.
- **Sharding** — `@//src/mongo/db/s:sharding_runtime_d` is one target bundling the
  *required* `CollectionShardingRuntime` / `DatabaseShardingRuntime` /
  `CollectionShardingStateFactoryShard` (shard_role calls into these on every collection
  access) together with resharding, the balancer, DDL coordinators and the transaction
  coordinator. It needs splitting into an "essentials" target. Expect resistance:
  `collection_sharding_runtime.cpp:15` includes `mongo/db/s/range_deleter_service.h`, which
  drags more of `db/s` back in. The mock factories in
  `src/mongo/db/shard_role/shard_role_loop_test.cpp` are **not** a shortcut — their `make()`
  is `MONGO_UNREACHABLE`, which is fine for that test but not for a running server.
- **Replication** — `repl_coordinator_impl` arrives transitively; the code already installs
  `ReplicationCoordinatorMock` from the separate `replmocks` target. Removing sharding may
  drop part of this for free — re-measure rather than assuming.

After all three: **~41 MB**, against a 35.7 MB floor.

Below ~41 MB there is no next big win. Once ICU (3.37 MB), Intel decimal128 (2.07 MB) and
WiredTiger (1.94 MB) are accounted for, no remaining bazel target exceeds 2.3 MB; the
mapped 45.8 MB is spread across hundreds of targets. Reaching 25 MB means cutting the query
engine itself — SBE, the aggregation pipeline, the command surface — which would also remove
`embedded_mongodb::Collection::aggregate` from this crate's public API.

## For the 64 GB machine: LTO

**This is the one remaining lever that needs no submodule patches.** It is untested here —
the attempt OOM-killed a 31 GB machine.

Do not repeat these flags:

```
--copt=-flto=8 --copt=-ffat-lto-objects --linkopt=-flto=8 --local_resources=cpu=16
```

`-ffat-lto-objects` emits both IR and machine code into every object, roughly doubling
5909 objects on disk and in the linker; GCC's LTRANS phase then ran 8-way on top of 16
parallel compiles. Start instead with:

```
--copt=-flto --linkopt=-flto=4 --linkopt=-Os --local_resources=cpu=8
```

No fat objects. Keep LTRANS parallelism (`-flto=N` at link) well under the core count and
watch RSS during the link — the WPA phase is single-threaded and holds the whole callgraph.
Raise `-flto=N` only after one link has completed inside memory.

Realistic expectation is 5–10% (≈49–51 MB), not a step change: `--gc-sections` and
`--icf=all` have already removed much of what LTO would find. Measure before building
anything on top of it.

### ThinLTO is not available on this checkout

Clang would be the better LTO path, but both routes are blocked:

- **System clang 22 + libstdc++ 16** fails to compile MongoDB —
  `src/mongo/db/timeseries/bucket_catalog/flat_bson.cpp` hits "call to deleted constructor of
  `mongo::tracking::Allocator<char>`" in `basic_string.h:822`. A pre-existing incompatibility,
  not caused by any flag here. An older `--gcc-toolchain`, or `-stdlib=libc++`, might get
  past it; neither was tried.
- **MongoDB's own hermetic clang toolchain is commented out** in this pinned submodule —
  `MODULE.bazel:287-292`, `TODO SERVER-122886: port over from WORKSPACE`. So
  `--//bazel/config:thin_lto=True` cannot resolve a toolchain; dropping
  `--config=native_toolchain` fails analysis with "Unable to find a CC toolchain using
  toolchain resolution".

If the submodule is ever bumped past SERVER-122886, retry ThinLTO first — it is both
cheaper and more effective than GCC LTO.

## Suggested order of work

1. **GCC LTO** with the corrected flags above. No patches, reversible, measure it.
2. **ICU via a trimmed `.dat`** — largest single object, and regenerating the blob keeps
   collation working instead of removing it.
3. **Sharding essentials split** — biggest raw win (−6.0 MB) but the most build-graph work.
4. **Replication** — re-measure after (3); part of it may already be gone.

Use the params-file technique to price each step before committing to a 40-minute rebuild,
and re-run `cargo test --release --all-targets` after each one.

## Reproducing a build

`embedded-mongodb-sys/build.rs` drives this automatically for `cargo build --release`. To
drive bazel directly (useful for experiments):

```sh
cd mongo
bazel build @mongot_localdev//:libembedded_mongodb_native.so \
  --override_repository=mongot_localdev=$PWD/../embedded-mongodb-sys/native \
  --config=opt --//bazel/config:opt=size \
  --//bazel/config:ssl=False --//bazel/config:build_otel=False \
  --//bazel/config:build_enterprise=False \
  --fission=no --debug_symbols=False \
  --copt=-fvisibility=hidden --copt=-ffunction-sections --copt=-fdata-sections \
  --linkopt=-Wl,-z,defs,--strip-all --linkopt=-Wl,--gc-sections \
  --linkopt=-Wl,--icf=all --linkopt=-Wl,-z,pack-relative-relocs \
  --config=native_toolchain --compiler_type=gcc \
  --//bazel/config:allocator=system --disable_warnings_as_errors=True \
  --copt=-include --copt=sys/syscall.h --copt=-fPIC
```

Test a library built outside cargo with
`EMBEDDED_MONGODB_NATIVE_LIB_DIR=<dir> cargo test --release --all-targets`, where `<dir>`
contains `libembedded_mongodb_native.so`.

A full rebuild is ~2400 s at `--local_resources=cpu=20` on 24 cores. Relinks are ~1 s.
