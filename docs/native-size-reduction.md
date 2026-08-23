# Shrinking `libembedded_mongodb_native.so`

Goal: ship the engine inside a Java library. Target 25 MB, hard ceiling 50 MB.

The release library is **42,317,408 bytes**, down from 147,116,136 when this work started
(−71%). It is under the 50 MB ceiling. No engine functionality was removed to get there:
the only thing dropped is collation data for 85 locales, and only in the patched build.

Two configurations are supported, and both pass `cargo test --release --all-targets`:

| | Size | vs. previous round |
| --- | --- | --- |
| Build flags only | 44,885,600 | −17.2% |
| With the patches in `patches/` | **42,317,408** | −21.9% |

All sizes are bytes from `stat -c%s`, on `x86_64-linux`, gcc 16.2.1, GNU ld 2.46, lld 22.1.8,
glibc 2.43. The intermediate steps below were measured on gcc 16.1.1, before a system upgrade
moved the toolchain mid-round; 16.2.1 produces a library about 160 KB smaller, so the rows do
not chain to the byte.

## Where the size went

Everything above the rule was established in earlier rounds and is unchanged.

| Step | Size | Δ |
| --- | --- | --- |
| Original | 147,116,136 | — |
| `-ffunction-sections -fdata-sections` + `--gc-sections --icf=all` | 101,593,920 | −31% |
| `-Wl,-z,pack-relative-relocs` (DT_RELR) | 96,509,736 | −5.1 MB |
| `--//bazel/config:opt=size` (`-Os` instead of `-O2`) | 61,956,680 | −34.6 MB |
| `ssl=False`, `build_otel=False`, `build_enterprise=False` | 54,050,888 | −7.9 MB |
| — rebuilt here on gcc 16.1.1 — | 54,216,296 | |
| `--version-script`, exporting only the five entry points | 52,496,088 | −1.72 MB |
| GCC LTO, linked with `ld.bfd` | 44,885,600 | −7.53 MB |
| ICU collation trim (`patches/0001`) | **42,317,408** | −2.57 MB |

By section:

| | Before | After |
| --- | --- | --- |
| `.text` | 30,578,530 | 24,504,175 |
| `.rodata` | 14,147,808 | 10,827,168 |
| `.eh_frame` | 3,174,648 | 2,611,716 |
| `.gcc_except_table` | 1,882,743 | 1,065,236 |
| `.data.rel.ro` | 1,676,208 | 1,630,768 |

## Export only what the library is for

`-fvisibility=hidden` left **5,915 symbols exported**: cpptrace, libdwarf, WiredTiger and asio
all mark their public API `visibility("default")` in their own headers, which beats a
command-line default. Every exported symbol is a `--gc-sections` root, so the linker was being
told to keep all of it.

`embedded-mongodb-sys/native/export.map` restricts the dynamic symbol table to the five
`embedded_mongodb_*` entry points. That is worth 1.72 MB on its own, and it is what lets LTO
internalize almost the entire program — the two compound.

## LTO

**The previous round's conclusion that LTO is impractical here was wrong**, for two reasons
that are easy to mistake for each other.

*It is not expensive.* The whole-program phase peaks at **7 GB** and the link takes **3
minutes**. What OOM-killed the 31 GB machine was `-ffat-lto-objects`, which emits both IR and
machine code into all 5,909 objects. Without it there is no memory problem to manage.

*lld cannot link GCC LTO objects.* GCC puts its IR in `.gnu.lto_*` sections and expects the
linker to call `liblto_plugin.so`; lld does not implement that plugin interface. It does not
report an error. It reads objects whose symbol tables contain only undefined references,
resolves nothing, and writes a **3,928-byte** shared library, which bazel accepts as a
successful build. If an LTO build suddenly produces a tiny library, this is why.

So the LTO link has to be `ld.bfd`, which has no identical code folding. That sounds
expensive — without LTO, `--icf=safe` costs **2.56 MB** over `--icf=all` — but the cost is
imaginary. Linking the same LTO objects with `mold`, the one linker that carries both the GCC
plugin and `--icf=all`, folding is worth **256 bytes**: 43,658,744 with `--icf=all` against
43,659,000 without. GCC's own `-fipa-icf` has already folded everything the linker would find.
What pre-LTO `--icf=all` was buying is exactly what LTO buys, not something lost to it.

Three toolchain features have to be switched off, not overridden:

```
--copt=-flto --linkopt=-flto=4 --linkopt=-Os --linkopt=-fuse-ld=bfd
--features=-linker_lld --features=-default_linker_lld --features=-supports_start_end_lib
```

`--linkopt=-fuse-ld=bfd` alone does nothing: mongo's toolchain appends its own `-fuse-ld=lld`
*after* the user link options, and GCC takes the last one. `-supports_start_end_lib` has to go
because bfd rejects `--start-lib`.

`mold` 2.41 links the LTO objects correctly and emits DT_RELR, and it is **1.40 MB worse**
than bfd on identical input: 43,658,744 against 42,263,616. (Those two, and the ICF pair above,
were all measured on one set of objects — the `-fno-asynchronous-unwind-tables` build below —
so they compare linkers, not configurations.) It leaves `.rodata.str1.1` at
6,483,950 beside a 5,481,120 `.rodata`, 11.97 MB of string data against bfd's merged 10.82 MB.
bfd tail-merges string constants — folds a string that is the suffix of another onto its tail —
and mold does not. On a codebase with this much static text that is worth more than everything
ICF returns. It is bfd's default, not a flag: `-Wl,-O2` changes the output by zero bytes here.
(Under lld, before LTO, the same flag was worth 59,840 bytes, which is why it was tried.)

`gold` also carries the plugin and has `--icf=all`, but the link fails on mongo's
`-Wl,--wrap=__cxa_throw`: the LTRANS objects reference `__wrap___cxa_throw` and gold does not
resolve it.

Clang/ThinLTO remains blocked exactly as recorded before — `flat_bson.cpp` does not compile
against libstdc++ 16, and the hermetic clang toolchain is commented out of this pinned
submodule (`MODULE.bazel:287-292`, SERVER-122886).

## The patch: ICU collation data

`patches/0001-trim-icu-collation-locales.patch`, applied with `scripts/apply-mongo-patches`
together with `patches/0002`, which fixes a crash described further down.

`icudt57l.dat` is 3,372,768 bytes compiled into the library as a byte array by
`generate_icu_init_cpp.py`, and it is the single largest object in the link. It holds 114
items, of which **111 are collation tables and only 3 are not** (`nfkc.nrm` for normalization,
`rfc4013.spp` for SASLprep, `rfc4518.spp` for X.509 DN normalization). Chinese alone is 918 KB,
Korean 290 KB, Japanese 146 KB.

The patch teaches that generator to rewrite the package before emitting it, keeping the three
non-collation items, the root tables (`root.res` plus `ucadata.icu`, 717 KB, the base every
locale builds on) and the sub-4 KB entries, which are aliases rather than tables of their own.
Result: 806,656 bytes, **−2,566,112**. Exactly 25 locales still work, verified by asking the
built library to create a collection under each: `am bg bo chr dz el en en_US fr fr_CA ga id it
ka lb lo mn ms ne nl pt ru sw wae zu`. `zh_Hant` survives the trim as an entry but aliases onto
the `zh` tables that were dropped, so it now fails like any other missing locale.

`KEEP_LOCALES` at the top of the patched file is the knob. Adding `"de"` costs 20 KB, `"es"`
25 KB, `"ja"` 146 KB.

A dropped locale degrades cleanly rather than crashing: `CollatorFactoryICU::validateLocaleID`
compares the request against ICU's `ULOC_VALID_LOCALE` and rejects any mismatch, so
`{locale: "zh"}` returns `Field 'locale' is invalid`, the same error an ICU-unknown locale
always produced. Note that `{locale: "root"}` is rejected too, and always was — ICU reports the
root collator's valid locale as the empty string. `tests/helpers.rs` asserts that `en` at
strength 2 still matches case-insensitively.

Severing the dependency instead would save 3.98 MB, but takes collation and SASLprep with it.

## Dead ends, with their numbers

Worth recording so nobody re-runs them.

- **`--skip_archive=False`** looks like the switch that turns off `alwayslink` globally —
  `mongo_cc_library` really does set `alwayslink = SKIP_ARCHIVE_ENABLED`. It is a no-op:
  54,218,408, +2 KB. `extract_debuginfo` re-wraps all 924 archives in `--whole-archive`.
- **Lazy-linking everything** (drop `--whole-archive`) gives 32,444,832 and a library that
  dies on first use with `BadValue: node BeginStartupOptionHandling was mentioned but never
  added`. This replaces the old "35.7 MB floor" estimate with two real numbers: 32.4 MB is what
  survives if registration-only objects are discarded, and **50,697,760** is what survives once
  the static-constructor objects are forced back in with `-u` (2,670 of them; another 107
  export only weak symbols and cannot be pinned that way). The 18 MB between
  them is feature registration — commands, aggregation stages, services — not dead code.
  2,777 of 4,265 objects carry a static constructor.
- **Splitting sharding out of `db/s` and `mongo/s`** is worth −5.54 MB only if dangling
  references are permitted. The link-clean subset needs 165 of 373 objects and is worth
  −2,365,568. A `LAZY_LINK_PACKAGES` list in `mongo_src_rules.bzl` expresses that in seven
  lines by setting `alwayslink = False` per package, but it does not compose with LTO: bfd
  resolves lazy archives before LTRANS, so references that only appear after optimization
  (`ReshardingMetrics::onWriteDuringCriticalSection`, `CancelState::abort`) come out undefined.
  LTO reclaims the same code and 5 MB more, so this was dropped.
- **Dropping SBE** measures −5.83 MB with dangling references allowed, the largest single
  candidate left. It is redundant with the classic execution engine rather than a feature, so
  it is the one worth pricing honestly if another 5 MB is ever needed — it would need
  `internalQueryFrameworkControl=forceClassicEngine` pinned at startup as well.
- **Removing Intel decimal128** (2,070,644 mapped) measures *larger*, not smaller. Probes that
  drop objects need `--unresolved-symbols=ignore-all`, which suppresses some GC; for cuts under
  a few MB the probe is not trustworthy in the small direction.
- **`-fno-asynchronous-unwind-tables`** looked promising against a 3.19 MB `.eh_frame` plus
  `.eh_frame_hdr`, and is worth 131,072 bytes: 42,263,616. Nearly every function in a codebase
  this exception-heavy needs its unwind entry for correctness, so only the leaves go. Not worth
  the loss of backtraces from signal handlers.
- `--//bazel/config:libunwind=off` and friends are below 0.5 MB combined. SpiderMonkey, curl
  and mongot are already absent from the link.

## Technique: price a cut without rebuilding

Bazel writes the full link command to a params file. Editing it and relinking takes about one
second against 40 minutes for a rebuild, and under LTO about three minutes.

```sh
E=~/.cache/bazel/_bazel_$USER/<hash>/execroot/_main
P=$E/bazel-out/k8-opt/bin/external/mongot_localdev/libembedded_mongodb_native.so-2.params

# Drop objects matching a pattern, retarget the output, allow the resulting dangling refs.
grep -vE '_objs/(sharding_runtime_d|transaction_coordinator)_with_debug/' $P \
  | sed 's|^bazel-out/.*/libembedded_mongodb_native.so$|/tmp/probe.so|;
         s|^-Wl,-z,defs,--strip-all$|-Wl,--strip-all|' > /tmp/probe.params
printf -- '-Wl,--unresolved-symbols=ignore-all\n-Wl,--noinhibit-exec\n' >> /tmp/probe.params
(cd $E && g++ @/tmp/probe.params) && stat -c%s /tmp/probe.so
```

Object paths carry a `_with_debug` suffix that the target name does not.

For an honest number, keep `-Wl,-z,defs` instead, drop the whole target, and add objects back
until the link succeeds: parse `undefined symbol:` out of the linker's stderr, map each name to
the object that defines it with `nm -gC --defined-only`, and repeat. It converges in about
eight rounds. GNU `nm` prints `> >` where LLVM prints `>>`, so compare names with the spaces
removed.

Add `-Wl,-Map=/tmp/link.map` for per-object attribution. The map is ~116 MB; aggregate it on
the `_objs/<target>/` path component rather than reading it.

## What is left, and why cutting subsystems does not work

The obvious next step is to remove what an embedded single-node engine cannot use: the
slot-based execution engine, sharding, replication, authentication, the network stack. Those
were each measured, and the answer is that build configuration cannot deliver any of them.

Their ceilings are real. Dropping each subsystem's objects from the LTO link while permitting
the resulting dangling references, against a 42,317,408 baseline:

| Cut | Result | Ceiling |
| --- | --- | --- |
| SBE | 36,960,704 | −5.36 MB |
| sharding + routing | 37,581,824 | −4.74 MB |
| replication | 40,108,256 | −2.21 MB |
| network stack | 41,073,280 | −1.24 MB |
| auth + crypto | 41,330,624 | −0.99 MB |

Those are ceilings, not savings, and three separate mechanisms fail to reach them.

**Dropping unreferenced objects buys almost nothing**, because LTO already did it. Rebuilding
each subsystem's minimal link-clean subset — drop the whole thing, add back only the objects
that define symbols the link then reports undefined — leaves 120 of SBE's 126 objects in place,
worth 22,624 bytes. Sharding is the exception at −2.97 MB, and that is pre-LTO; LTO collects the
same code.

**Following the reference cascade destroys the engine.** Seeding SBE's 126 objects and
repeatedly adding every object that references something already dropped converges on **481
objects**, including 80 in core `src/mongo/db`, 91 in `db/s` and 22 in `db/commands`.

**Constant-folding the engine choice does nothing.** `QueryKnobConfiguration` reaches the
executor through one generated accessor, so shadowing it with a constant `kForceClassicEngine`
makes every engine-choice branch provably dead and should let LTO strip SBE for free. It
rebuilt 158 translation units and produced a library of *exactly the same size*, because SBE
stays reachable through the plan cache, `plan_executor_factory` and explain. Forcing the
classic engine at runtime costs query performance and saves nothing, so it was reverted too.

What would work is a fork. Only 23 objects outside SBE reference it, and most are themselves
SBE-specific (`plan_executor_sbe`, `plan_explainer_sbe`, `sbe_trial_runtime_executor`, the
deferred-engine-choice planners); the genuinely shared ones are about nine files that branch on
the engine and would have to be patched to take the classic path unconditionally. Note that
five of the 23 are histogram and cardinality-estimation files using `sbe::value` purely as a
data representation, so `sbe_values` has to stay whatever else goes.

That is a fork of MongoDB's query execution layer, carried against a pinned submodule,
re-applied on every bump, in the code path this crate's users exercise most. The other four
subsystems each need their own, and sharding's is worse because `shard_role` is on every
collection access. Worth roughly 14 MB if all five landed. Not attempted.

Below that, no remaining bazel target exceeds 2.3 MB. By directory the largest are
`src/mongo/db` 5.16 MB, `db/pipeline` 3.85 MB, sharding 3.40 MB, SBE 2.85 MB, `db/repl`
1.69 MB. Intel decimal128 is 2.07 MB and WiredTiger 1.94 MB, both required.

25 MB still means cutting the query engine, and with it
`embedded_mongodb::Collection::aggregate` from this crate's public API.

## Two crashes this work found

Neither was caused by the size work; both predate it, and `tests/features.rs` covers them now.
Both took the host process down with an fassert rather than returning an error, which is the
worst failure mode a library embedded in someone else's application can have.

- **`explain`** reports server version information, and nothing had ever called
  `VersionInfoInterface::enable()`. mongod gets it from a static initializer in
  `//src/mongo/util:version_impl`, which `native/BUILD.bazel` did not depend on. Adding the dep
  fixes it.
- **The first `hello`** — `Client::getRemote()` verifies that a session exists, and a client
  created in-process has none. Recording client metadata logs the remote address, so the first
  handshake aborted; later ones succeeded because metadata is only recorded once.
  `patches/0002` guards the two logging sites in `client_metadata.cpp` and the one in
  `hello_auth.cpp`. This is what a driver sends first, so it hit the PyMongo bindings directly.

## Reproducing

```sh
git submodule update --init --depth 1
./scripts/apply-mongo-patches          # 2.57 MB, plus a crash fix
cargo build --release
```

`embedded-mongodb-sys/build.rs` passes the flags above. To drive bazel directly:

```sh
cd mongo
bazel build @mongot_localdev//:libembedded_mongodb_native.so \
  --override_repository=mongot_localdev=$PWD/../embedded-mongodb-sys/native \
  --config=opt --//bazel/config:opt=size \
  --//bazel/config:ssl=False --//bazel/config:build_otel=False \
  --//bazel/config:build_enterprise=False \
  --fission=no --debug_symbols=False \
  --copt=-fvisibility=hidden --copt=-ffunction-sections --copt=-fdata-sections \
  --copt=-flto --linkopt=-flto=4 --linkopt=-Os --linkopt=-fuse-ld=bfd \
  --features=-linker_lld --features=-default_linker_lld \
  --features=-supports_start_end_lib \
  --linkopt=-Wl,-z,defs,--strip-all --linkopt=-Wl,--gc-sections \
  --linkopt=-Wl,-z,pack-relative-relocs \
  --config=native_toolchain --compiler_type=gcc \
  --//bazel/config:allocator=system --disable_warnings_as_errors=True \
  --copt=-include --copt=sys/syscall.h --copt=-fPIC
```

Test a library built outside cargo with
`EMBEDDED_MONGODB_NATIVE_LIB_DIR=<dir> cargo test --release --all-targets`, where `<dir>`
contains `libembedded_mongodb_native.so`.

A full rebuild is about 45 minutes at `--local_resources=cpu=18` on 20 cores. Changing only
link flags or `alwayslink` reuses every compile action and takes about five minutes.
