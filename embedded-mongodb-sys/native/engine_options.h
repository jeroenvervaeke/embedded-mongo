#pragma once

#include "embedded_mongodb_native.h"

#include <cstdint>
#include <string>

namespace embedded_mongodb {

/// One `embedded_mongodb_open_options` with every zero replaced by this library's default and
/// every value checked against what WiredTiger will accept. Nothing downstream of
/// `resolveOptions` has to think about the caller's struct again.
struct ResolvedOptions {
    std::uint32_t cacheSizeMB;
    std::uint32_t journalFileMaxKB;
    bool journalPrealloc;

    /// The `wiredtiger_open` fragment that carries the journal settings, comma-terminated so
    /// that whatever MongoDB appends after it stays a separate configuration entry.
    std::string wiredTigerJournalConfig() const;
};

/// Throws `mongo::DBException` if a field this library understands is outside the range
/// WiredTiger accepts, so a bad value is a failed open rather than a failed write later.
///
/// `options` may be null, and `options->size` may describe a struct shorter than this build's:
/// see the contract on `embedded_mongodb_open_options`.
ResolvedOptions resolveOptions(const embedded_mongodb_open_options* options);

/// Writes the resolved values into the MongoDB globals the storage engine reads on startup.
/// Must run before `startUpStorageEngineAndCollectionCatalog`, which is where WiredTiger is
/// opened and where every one of these is read for the last time.
void applyOptions(const ResolvedOptions& options);

}  // namespace embedded_mongodb
