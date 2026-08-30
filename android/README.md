# embedded-mongodb-android

The Android library: MongoDB running inside the application process, with no server, no socket
and no `mongod` to install. It wraps the `embedded-mongodb-android` Rust crate in a Kotlin API and
packages both into an AAR.

```kotlin
// Inside a coroutine: opening and every query suspend, and all of them run on the library's own
// database thread rather than on the caller's.
val mongo = EmbeddedMongo.open(context, File(context.filesDir, "shop"))
val orders = mongo.getDatabase("shop").getCollection("orders")

orders.insertOne(Document("customer", "ada").append("total", 12).append("paid", false))
orders.createIndex(Indexes.ascending("customer"))

val unpaid = orders.find(Document("paid", false))
    .sort(Document("total", -1))
    .limit(20)
    .toList()
```

An instance is meant to outlive a single screen — open it once, keep it for as long as the data is
in use, and `close()` it off the main thread when it is not.

## Two modules, one dependency

| | |
| --- | --- |
| `:embedded-mongodb-core` | plain Kotlin: databases, collections, queries, cursor paging, and the commands they build |
| `:embedded-mongodb` | the AAR: the engine, the JNI bridge, opening, closing, storage limits, and the main-thread guard |

An application depends on the AAR alone — it re-exports the core module, so the whole API arrives
with it. The split is what lets a plain-Kotlin module of an application build a query without
taking on the Android SDK or a compiled engine, and it is why the query layer is tested on the JVM.

## Databases, collections and queries

The seam between the two modules is one interface, and every collection and query in the library
is written against it:

```kotlin
fun interface CommandRunner {
    suspend fun runCommand(database: String, command: Document): Document
}
```

`EmbeddedMongo` is the implementation that reaches the engine. `MongoDatabase(runner, name)` has a
public constructor, so a test — or a wrapper that traces, logs or retries — gets the whole query
API with five lines of its own.

**On a collection.** `find`, `aggregate`, `distinct`, `countDocuments`,
`estimatedDocumentCount`, `insertOne`, `insertMany`, `updateOne`, `updateMany`, `replaceOne`,
`findOneAndUpdate`, `findOneAndReplace`, `findOneAndDelete`, `deleteOne`, `deleteMany`,
`createIndex`, `createIndexes`, `listIndexes`, `dropIndex`, `drop`.

**On a database.** `getCollection`, `listCollectionNames`, `createCollection`, `drop`.

The names are the driver's names and so is the behaviour, including where MongoDB reports "there
was nothing to do": `drop` on a collection or database that is not there is a no-op, while
`createCollection` on one that exists and `dropIndex` on an index that does not are failures
carrying [MongoErrorCode] values to catch. That is what the Java and Kotlin drivers do, and an
application using create-or-fail as a race guard needs it.

All but `find`, `aggregate` and the two `runCommand`s are extension functions rather than members,
and the rule is worth knowing because it is the one an application's own operations follow: a
member is what needs the collection's private command runner, and everything else is written on
`runCommand` exactly as you would write one. From Java they are static methods on
`MongoCollections`.

`findOneAndUpdate` and `findOneAndDelete` are the only atomic read-modify-writes here, which is
why they exist rather than being left to `runCommand`: an `updateOne` followed by a `find` is two
commands, and two coroutines really do interleave between them.

`find` and `aggregate` return a query rather than results. Nothing reaches the engine until it is
collected, every method on it returns a new query rather than changing the one it was called on,
and `command()` reports what would be sent — which is what an application shows when it wants to
prove that the pipeline on the screen is the pipeline that ran.

A query **is** a `Flow<Document>`, as the driver's `FindFlow` and `AggregateFlow` are, so it can
be collected directly and every `Flow` operator applies:

```kotlin
val paid = orders.find(Document("paid", true))
val newest = paid.sort(Document("placed", -1)).limit(10).toList()
paid.collect(::render)                        // it is the flow; no asFlow() needed
val mine = paid.asFlow(Document::toOrder)     // parsed as it arrives
```

There is no `Collection<T>`. `asFlow(read)` is the whole of the typed story: parsing is a function
from a `Document`, and where a document becomes a domain object is a boundary an application owns
anyway. The machinery for a generic parameter is available — `PojoCodecProvider` and
`CodecRegistries` are in `org.mongodb:bson`, which is already a dependency — so this is a choice
rather than a limitation: a codec registry is a second way to describe a document, and the one
place it would earn its keep, Kotlin data classes, is the place the POJO codec handles worst.

`insertOne` and `insertMany` generate an `ObjectId` for a document that names no `_id`, and report
what each document was stored under. Writes are journalled — see [Durability](#durability).

`drop`, `dropIndex` and `createCollection` treat MongoDB's "there was nothing to do" codes as
success: asking for a collection to be gone when it is already gone is not a failure.

### Filters, sorts, projections and updates

Every one of them is an `org.bson.conversions.Bson`. `Document` implements it, so
`Document("paid", true)` is a filter and nothing else is needed:

```kotlin
orders.updateMany(Document("paid", false), Document("\$set", Document("paid", true)))
```

It is also what the official driver's `Filters`, `Sorts`, `Projections`, `Updates`, `Aggregates`
and `Accumulators` builders return, so an application can add `org.mongodb:mongodb-driver-core` to
its own dependencies and use them at the same call sites — this library does not depend on the
driver to make that work.

**Adding it is the recommendation rather than an aside**, and pipelines are why. Kotlin gives `$`
to string templates, so every MongoDB operator written as a literal has to be escaped:
`Document("\$match", Document("total", Document("\$lte", 20)))`. One is tolerable; an
aggregation pipeline is a dozen. `Aggregates.match(Filters.lte("total", 20))` says the same thing
and cannot be got wrong by a missing backslash.

The one builder here is `Indexes`, because an index key specification is the part that is easy to
get quietly wrong: `Document("loc", "2dsphere")` and `Document("loc", 1)` differ by a value rather
than by a keyword, and the second builds an index `$geoNear` will not use.

```kotlin
places.createIndexes(
    listOf(
        IndexModel(Indexes.geo2dsphere("loc")),
        IndexModel(Indexes.text("name", "brand"), IndexOptions(weights = Document("name", 10))),
    ),
)
```

### Errors

One exception, `EmbeddedMongoException`, carrying MongoDB's own error code — so the check a
MongoDB developer already knows is the check to write:

```kotlin
try {
    orders.insertOne(order)
} catch (failure: EmbeddedMongoException) {
    if (failure.code != MongoErrorCode.DUPLICATE_KEY) throw failure
}
```

`response` is the whole reply the engine sent, and `writeErrors` reads the per-document failures
out of it. That is what makes `insertMany(ordered = false)` worth asking for: the engine carries
on past a document it rejects, so the exception holds every rejection rather than only the one
that stopped the batch, and `response["n"]` says how many went in regardless.

A negative `code` is the bridge rather than MongoDB — `bridgeError` names it — and
`NO_ERROR_CODE` means this library raised the failure itself, which it does for a reply it cannot
parse.

### When none of it fits

`MongoDatabase.runCommand`, `MongoCollection.runCommand` and their `runCursorCommand` siblings send
a command written by hand and hand back the reply — `collStats`, `explain`, `distinct`, a command
MongoDB grew after this library did. `EmbeddedMongo.runCommand` and `runCommandBlocking` are the
same thing one level down, naming the database at the call site. They are the last resort rather
than the API, but everything above them ends up there too, so a command written by hand is not a
lesser citizen — only an unchecked one.

`explain` is the one worth spelling out, because `command()` makes it a one-liner:

```kotlin
val query = orders.find(Document("paid", true)).sort(Document("placed", -1))
val plan = shop.runCommand(Document("explain", query.command()).append("verbosity", "queryPlanner"))
```

## What is in the AAR

Three shared libraries per ABI:

| | |
| --- | --- |
| `libembedded_mongodb_android.so` | the JNI bridge, built from `embedded-mongodb-android` |
| `libembedded_mongodb_native.so` | the engine, downloaded by the sys crate's build script |
| `libc++_shared.so` | the NDK's C++ runtime, taken from the NDK sysroot |

The third is not optional. The engine links its own C++ runtime statically, but the `cxx` bridge
compiled into the Rust crate uses the NDK's shared one, as any other NDK library would, and without
it `System.loadLibrary` fails on the device.

`arm64-v8a` and `x86_64`, and nothing else: MongoDB has no 32-bit build, so a 32-bit install would
be a crash rather than a slower database. `abiFilters` names both, the Gradle task builds only
those two, and it reads the ELF header of every library it stages — a 32-bit or wrong-architecture
library fails the build rather than reaching a device.

The engine is most of the download. Applications that ship both ABIs should use an app bundle, so
Google Play delivers one.

## Using it from an application

The AAR is not published to a repository yet, so an application consumes the module itself —
`includeBuild` this directory, or copy `embedded-mongodb-release.aar` into the application's
`libs/`:

```kotlin
dependencies {
    // The AAR alone: it re-exports :embedded-mongodb-core, so the whole query API comes with it.
    implementation(project(":embedded-mongodb"))
}

android {
    defaultConfig {
        minSdk = 26

        // Only if the application ships other native code: an AAR's ABI list does not reach the
        // application's, so an unrelated 32-bit library would produce an armeabi-v7a split that
        // installs without an engine to load.
        ndk { abiFilters += setOf("arm64-v8a", "x86_64") }
    }
}
```

`minSdk` 26 — Android 8.0 — is the floor, and it is verified on a device rather than claimed: the
23 instrumented tests pass on an API 26 emulator, and the same suite on API 35 fails nothing that
API 26 does not.

The floor is set by `org.bson`, not by the engine. The engine is compiled against bionic at API
level 24 and does run there — on an API 24 device it loads, opens a database and answers commands.
But `Document` is in every signature here, so `org.bson` is an `api` dependency, and
`org.bson.conversions.Bson`'s static initializer builds the JSR-310 codecs:

```
java.lang.NoClassDefFoundError: Failed resolution of: Ljava/time/Instant;
    at org.bson.codecs.jsr310.InstantCodec.getEncoderClass(InstantCodec.java:64)
    at org.bson.codecs.jsr310.Jsr310CodecProvider.<clinit>(Jsr310CodecProvider.java:44)
    at org.bson.conversions.Bson.<clinit>(Bson.java:61)
```

`java.time` arrived in API 26, so on 24 and 25 the first call that touches a `Document` dies there,
before it reaches the engine. Core library desugaring exists to backport `java.time`, but it is not
transitive — an AAR cannot enable it for the application consuming it — so 26 is the lowest floor
this module can honour on its own.

One caveat for anyone wiring this into CI: an API 24 device cannot run
`:embedded-mongodb:connectedDebugAndroidTest` at all, whatever the floor says. AGP installs through
ddmlib's split installer, and `install-multiple` fails on Android 7.0 with `failed to finalize
session`; it works from API 26 up. Below 26 the suite has to be driven by hand with
`adb install -t -r` and `am instrument -w`, which is how the API 24 evidence above was gathered.

One database is open per process. The engine refuses a second runtime, so an application keeps
one `EmbeddedMongo` and uses database names inside it, exactly as it would against a server.

`:embedded-mongodb-core`, `org.mongodb:bson` and `kotlinx-coroutines-core` all arrive
transitively, because the collection API, `Document` and `Flow` are in the API. The MongoDB Java driver is deliberately not a dependency — there is no server to
connect to, and its connection pooling, topology monitoring and retry machinery have nothing to do.

### Turn off backup for the database directory

This is the one thing an application **must** do. With the default `allowBackup="true"`, Android
uploads application-private files to the user's Google Drive and restores them onto whatever device
and whatever build of the application comes next. A restored database is a WiredTiger data
directory written by a different engine build on a different machine, which is a corrupt database
rather than a migrated one.

Either turn backup off entirely:

```xml
<application android:allowBackup="false" />
```

or keep it and exclude the directory the database lives in — both files, because the format
changed in API 31:

```xml
<application
    android:dataExtractionRules="@xml/data_extraction_rules"
    android:fullBackupContent="@xml/backup_rules" />
```

```xml
<!-- res/xml/data_extraction_rules.xml, API 31 and above -->
<data-extraction-rules>
    <cloud-backup>
        <exclude domain="file" path="shop" />
    </cloud-backup>
    <device-transfer>
        <exclude domain="file" path="shop" />
    </device-transfer>
</data-extraction-rules>
```

```xml
<!-- res/xml/backup_rules.xml, API 30 and below -->
<full-backup-content>
    <exclude domain="file" path="shop" />
</full-backup-content>
```

`path` is relative to `context.filesDir` for `domain="file"`, so `shop` above is the directory in
the example at the top. Name whichever directory you passed to `EmbeddedMongo.open`.

### R8 and minified builds

Nothing to do: the AAR carries `consumer-rules.pro`. It keeps the JNI entry points, which R8 would
otherwise rename into an `UnsatisfiedLinkError`, and the BSON codec classes, which are found
reflectively and would otherwise be stripped.

## Threads

The engine runs every command on one internal strand, so the library dispatches onto a single
thread of its own rather than a pool: a second thread would only queue behind the first.

- Every query, every write and `EmbeddedMongo.open` are suspending and run there. So is
  `runCommand`. **The collection API is coroutines-only** — there are no `…Blocking` twins of it,
  because a blocking mirror of every operation is a second API to keep honest. A caller with no
  coroutine to run in (`Worker.doWork`, Java) wraps one call in `runBlocking` off the main thread.
- `runCommandBlocking` and `openBlocking` run on the calling thread and **throw** on Android's main
  thread. A query over a few thousand documents outlasts the ANR budget, and neither the engine nor
  JNI can interrupt one.
- `close` warns instead of throwing, because closing from a lifecycle callback is reasonable and an
  exception thrown out of `use { }` would replace whatever sent the caller there.

A cursor is a `Flow`, and a collector that stops early — `take`, `first`, a cancelled scope, an
exception — kills the cursor on its way out. Nothing has to be closed by hand, and the engine is
not left holding the storage snapshot a cursor reads from.

Failed commands are exceptions: `EmbeddedMongoException` carries the reply's `errmsg` and `code`.
A reply with `ok: 0`, a populated `writeErrors`, or a `writeConcernError` all raise it, so an insert
that stored nothing cannot read as a success.

`code` says where the failure came from. A positive number is a MongoDB error code. A negative one
is the bridge itself, and `EmbeddedMongoException.bridgeError` names it — `UNKNOWN_HANDLE` for a
database that is already closed, `PANIC` for a Rust panic caught at the boundary, `ENGINE_ERROR`
for an engine failure that carried no number. Zero means this library raised the failure, which it
does for a reply it cannot parse.

## Durability

Writes are journalled before they are acknowledged. MongoDB's own default acknowledges a write as
soon as it is in memory, which on a platform that ends processes without warning loses the last
few hundred writes — and an insert that implicitly created a collection can take the collection
with it. So a write command that names no `writeConcern` is sent with `{w: 1, j: true}` — the
writes the collection API builds included. A caller who wants the faster, lossy behaviour puts
their own `writeConcern` in a command sent through `runCommand`.

## Storage limits

The defaults are already sized for a phone rather than for a server, and an application that names
nothing gets them: a directory holding 2.25 MiB of documents and indexes occupies 10.25 MiB here,
against 202 MiB at mongod's journal settings, nearly all of which is journal files allocated in
full whether or not anything is written to them. What is left to name is what only the application
can know.

| Limit | Default | Named by |
| --- | --- | --- |
| WiredTiger cache | 256 MB | `StorageOptions.cacheSize` |
| Journal file size | 8 MiB (mongod: 100 MB) | `StorageOptions.journalFileSize` |
| Journal pre-allocation | off (mongod: on) | `StorageOptions.journalPreallocation` |
| Free disk to start an index build or spill a query | 500 MB, as mongod | `StorageOptions.freeDiskFloor`, and `setFreeDiskFloor` at any time |

```kotlin
// Inside a coroutine, as with every other overload of open.
val database = EmbeddedMongo.open(
    context,
    File(context.filesDir, "shop"),
    StorageOptions(
        cacheSize = CacheSize.ofMebibytes(32),
        freeDiskFloor = FreeDiskFloor.ofMebibytes(64),
    ),
)
```

Every limit left `null` keeps the engine's own default, so `StorageOptions()` opens exactly the way
`open(context, directory)` does. Each one is a type rather than a number, and each is checked where
it is written: `CacheSize.ofMebibytes(0)` throws at that line rather than failing an open later, and
a `CacheSize` cannot be handed to the journal.

The first three are read once while WiredTiger is opening and cannot be changed afterwards. The
free-disk floor is a pair of server parameters, so it can also be moved on a database that is
already open — `setFreeDiskFloor` and `setFreeDiskFloorBlocking`, with `freeDiskFloors` reporting
what the engine is running with. Raising it before a large index build and dropping it afterwards
is a reasonable thing to do.

### The free-disk floor, and what lowering it costs

MongoDB will not start an index build, or spill a query to disk, with less than 500 MB free. That
is sized for a server. A phone near its limit does not have 500 MB free at all, so on such a device
an application that can open and read its database still cannot seed one: `createIndexes` fails
with `OutOfDiskSpace`. Lowering the floor is what makes that device work, and it is the reason this
knob is reachable from Kotlin at all.

It is also the only warning an application gets. The floor is a pre-flight check and nothing more:
it refuses a build that would *start* with too little room, and nothing stops one that runs out
part-way. This engine runs no `DiskSpaceMonitor` — the thread mongod uses to abort builds as a disk
fills is started from `mongod_main`, which this engine does not use — and WiredTiger answers a
genuinely full disk with `WT_PANIC`, which MongoDB answers with `fassert`. That aborts the
application process: no exception, no return value, nothing to catch.

So lowering the floor trades a clean refusal the application can report to the user for a crash it
cannot. Lower it to what the work about to be done actually needs, not to what will fit.

### Before the engine is opened at all

`EmbeddedMongo.open(context, directory)` checks that the volume can give the engine room to work
and throws `InsufficientStorageException`, which carries how much room there is and how much is
wanted. The overload without a `Context` cannot ask, so prefer the one with it.

The measurement is `StorageManager.getAllocatableBytes`, which counts the cached data Android will
delete for the application — `allocateBytes` reclaims that space rather than merely counting it. It
answers for the application rather than for the volume, so what it is held to (256 MiB) is lower
than the engine's own floor; this check is there to catch a device with nothing left, not to
second-guess the engine. A `StorageOptions.freeDiskFloor` below that lowers this check to match,
since an application that named a floor has already said how much room is enough for it — but
never below what opening itself costs, which is the journal file allocated in full (8 MiB by
default, twice that with a spare kept ready) plus a megabyte for everything else a fresh
directory holds. The floor governs index builds, not whether WiredTiger can create its first
journal file.

## Building this module

```sh
./gradlew build                            # both modules, unit tests, build logic tests, lint
./gradlew :embedded-mongodb-core:test      # the query layer: no SDK, no NDK, no engine
./gradlew :embedded-mongodb:testDebugUnitTest
./gradlew :embedded-mongodb:connectedDebugAndroidTest   # needs a 64-bit device or emulator
```

`:embedded-mongodb-core:test` needs none of what follows: it is plain Kotlin on the JVM, and every
query in it is exercised against a scripted `CommandRunner`.

The build needs:

- **JDK 21.** `gradle/gradle-daemon-jvm.properties` asks for it, and Gradle fails with a clear
  message when it finds none. Android Studio's bundled runtime qualifies; point Gradle at a JDK
  it does not discover on its own with `org.gradle.java.installations.paths` in
  `~/.gradle/gradle.properties`.
- **The Android SDK**, through `ANDROID_HOME` or `sdk.dir` in `local.properties`, with the NDK
  version pinned in `embedded-mongodb/build.gradle.kts`: `sdkmanager "ndk;<version>"`. The engine
  needs r27 or newer.
- **Rust**, with `rustup target add aarch64-linux-android x86_64-linux-android`. `cargo-ndk` is not
  used: the `cargoJniLibs` task passes the NDK's compiler, archiver and linker to cargo itself,
  which is the same set of variables the [root README](../README.md) documents.

`cargoJniLibs` is wired into the AAR through the variant's `jniLibs`, so the libraries cannot go
missing from a build that succeeds. It reruns when the Rust crates or the workspace lockfile
change, and is skipped otherwise.

Instrumented tests need a 64-bit image, and an image recent enough to be given a CPU model with
BMI1 and BMI2: the engine's x86_64 build uses those instructions, and an older emulator dies with
`SIGILL` inside `System.loadLibrary`. API 35 is what CI runs; `-qemu -cpu host` is the way out on
an older one.

CI runs all of it — `./gradlew build` and the instrumented tests on an API 35 emulator — in the
`android` job of [`ci.yml`](../.github/workflows/ci.yml). It installs the NDK version read out of
`embedded-mongodb/build.gradle.kts`, so bumping the pin there is all it takes to move CI too.
