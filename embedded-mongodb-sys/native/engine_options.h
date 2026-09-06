#pragma once

#include "embedded_mongodb_native.h"

#include <cstdint>
#include <string>

namespace embedded_mongodb {

/// One `embedded_mongodb_open_options` with every zero replaced by this library's default.
/// Numeric ranges are not re-checked here: the sole caller of this ABI is this project's Rust
/// crate, which validates each value against WiredTiger's bounds before building it (the
/// newtypes in `options.rs`). Nothing downstream of `resolveOptions` has to think about the
/// caller's struct again.
struct ResolvedOptions {
    std::uint32_t cacheSizeMB;
    std::uint32_t journalFileMaxKB;
    bool journalPrealloc;

    /// The `wiredtiger_open` fragment that carries the journal settings, comma-terminated so
    /// that whatever MongoDB appends after it stays a separate configuration entry.
    std::string wiredTigerJournalConfig() const;
};

/// Fills defaults and maps the journal-prealloc tri-state; throws `mongo::DBException` only if
/// that enum field holds a value outside its three cases. Numeric ranges are the Rust caller's
/// to enforce (see above).
///
/// `options` may be null, and `options->size` may describe a struct shorter than this build's:
/// see the contract on `embedded_mongodb_open_options`.
ResolvedOptions resolveOptions(const embedded_mongodb_open_options* options);

/// Writes the resolved values into the MongoDB globals the storage engine reads on startup.
/// Must run before `startUpStorageEngineAndCollectionCatalog`, which is where WiredTiger is
/// opened and where every one of these is read for the last time.
void applyOptions(const ResolvedOptions& options);

}  // namespace embedded_mongodb
