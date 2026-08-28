#include "engine_options.h"

#include "mongo/db/storage/wiredtiger/wiredtiger_global_options.h"
#include "mongo/db/storage/wiredtiger/wiredtiger_global_options_gen.h"
#include "mongo/util/assert_util.h"

#include <algorithm>
#include <cstddef>
#include <cstring>
#include <string>

namespace embedded_mongodb {
namespace {

// Only two settings live here, and they are here for the same reason: WiredTiger reads both
// while `wiredtiger_open` runs and neither can be reconfigured afterwards. Everything else
// MongoDB lets a server operator size is a server parameter, which a client reaches through
// `setParameter` without an engine rebuild -- see `limits.rs` in the safe layer.

// Unchanged from the fixed value this engine has always run with: `cacheSizeGB = 0.25` is
// 256 MB, which is also mongod's own floor for a server. It is a ceiling WiredTiger grows into
// rather than memory it takes, and a cold read-only process at Ireland scale peaks at 48 MiB
// resident, so lowering it would buy nothing measurable. It is settable because a device with
// a real memory budget should be able to state one, not because the default is wrong.
constexpr std::uint32_t kDefaultCacheSizeMB = 256;

// One journal file is allocated in full the moment WiredTiger creates it, so an empty
// database costs exactly this much before it holds anything. At mongod's 100 MB default a
// 2 MiB dataset costs 100 MiB, the single largest item in this engine's footprint. 8 MiB is
// small enough to disappear next to an application bundle and still comfortably above
// 2.5 MiB, the point below which WiredTiger shrinks its log slot buffers to file_max/10
// (log_slot.c) and starts pushing ordinary writes down the unbuffered path.
//
// It does not bound what the journal costs under sustained writing: log files are removed
// once a checkpoint makes them obsolete, so a burst holds a checkpoint interval's worth of
// records whatever the file size. It bounds what an idle database costs, which for an
// offline application is nearly all of the time.
constexpr std::uint32_t kDefaultJournalFileMaxKB = 8 * 1024;

// Pre-allocation keeps one spare journal file ready so that rolling over does not have to
// create one on the writing thread. That spare is a second full-size file on disk at all
// times -- half the journal footprint -- bought with latency these workloads do not notice.
//
// Durability is not part of the trade, and that claim is the reason this default was allowed
// to change: WiredTiger creates the file through the same __wti_log_allocfile either way
// (log.c), which writes the header, extends the file and fsyncs both the file and its
// directory before renaming it into place. Pre-allocation only moves that work off the
// writing thread. tests/durability is what actually settles it.
constexpr bool kDefaultJournalPrealloc = false;

// WiredTiger's own limits, from src/third_party/wiredtiger/src/config/config_def.c, where
// cache_size is "min=1MB,max=10TB" and log.file_max is "min=100KB,max=2GB". Checking them
// here turns a value WiredTiger would reject inside wiredtiger_open -- where it surfaces as
// an opaque EINVAL from a C library -- into a named error before anything is opened.
constexpr std::uint32_t kMinCacheSizeMB = 1;
constexpr std::uint32_t kMaxCacheSizeMB = 10 * 1000 * 1000;
constexpr std::uint32_t kMinJournalFileMaxKB = 100;
constexpr std::uint32_t kMaxJournalFileMaxKB = 2 * 1024 * 1024;

/// Copies as much of the caller's struct as the caller says exists, leaving everything past
/// it zero. Reading a member the caller never allocated would be a read off the end of their
/// object, so `size` has to gate the copy rather than the interpretation.
embedded_mongodb_open_options requested(const embedded_mongodb_open_options* options) {
    embedded_mongodb_open_options copy{};
    if (options) {
        // A caller who zeroed the struct, filled in fields and forgot `size` would otherwise
        // have every one of them discarded in silence -- the one way to misuse this struct
        // that looks exactly like asking for the defaults. Anything too short to hold `size`
        // itself cannot be a struct this function was handed on purpose.
        uassert(13180012,
                "embedded_mongodb_open_options.size must be set to sizeof the caller's struct, "
                "got " +
                    std::to_string(options->size),
                options->size >= sizeof(options->size));
        std::memcpy(&copy, options, std::min(options->size, sizeof(copy)));
    }
    copy.size = sizeof(copy);
    return copy;
}

std::uint32_t inRange(std::uint32_t value,
                      std::uint32_t low,
                      std::uint32_t high,
                      std::uint32_t fallback,
                      const char* name) {
    if (value == 0) {
        return fallback;
    }
    uassert(13180010,
            std::string("embedded MongoDB option ") + name + " must be between " +
                std::to_string(low) + " and " + std::to_string(high) + ", got " +
                std::to_string(value),
            value >= low && value <= high);
    return value;
}

bool resolvePrealloc(std::uint32_t value) {
    switch (value) {
        case EMBEDDED_MONGODB_JOURNAL_PREALLOC_DEFAULT:
            return kDefaultJournalPrealloc;
        case EMBEDDED_MONGODB_JOURNAL_PREALLOC_ENABLED:
            return true;
        case EMBEDDED_MONGODB_JOURNAL_PREALLOC_DISABLED:
            return false;
    }
    uasserted(13180011,
              "embedded MongoDB option journal_prealloc must be one of "
              "embedded_mongodb_journal_prealloc, got " +
                  std::to_string(value));
}

}  // namespace

std::string ResolvedOptions::wiredTigerJournalConfig() const {
    // Appended last to the wiredtiger_open configuration string, after the
    // log=(enabled=true,remove=true,path=journal,compressor=...) the persistence provider
    // contributes. WiredTiger resolves a dotted key by scanning every occurrence of its
    // parent and keeping the last leaf it finds (__config_getraw), so naming two members of
    // log here overrides exactly those two and leaves journalling, its directory and its
    // compressor as the provider set them. Comma-terminated because a restore configuration
    // can still be appended after it.
    return "log=(file_max=" + std::to_string(journalFileMaxKB) +
        "KB,prealloc=" + (journalPrealloc ? "true" : "false") + "),";
}

ResolvedOptions resolveOptions(const embedded_mongodb_open_options* options) {
    const auto asked = requested(options);
    return ResolvedOptions{
        .cacheSizeMB = inRange(asked.cache_size_mb,
                               kMinCacheSizeMB,
                               kMaxCacheSizeMB,
                               kDefaultCacheSizeMB,
                               "cache_size_mb"),
        .journalFileMaxKB = inRange(asked.journal_file_max_kb,
                                    kMinJournalFileMaxKB,
                                    kMaxJournalFileMaxKB,
                                    kDefaultJournalFileMaxKB,
                                    "journal_file_max_kb"),
        .journalPrealloc = resolvePrealloc(asked.journal_prealloc),
    };
}

void applyOptions(const ResolvedOptions& options) {
    // WiredTigerUtil::getMainCacheSizeMB multiplies this back by 1024 and floors it. Every
    // integer over 1024 is exact in binary, so the megabyte count survives the round trip.
    //
    // Its documented 256 MB minimum is reached only on the branch that computes a size from
    // system memory, i.e. only when nothing was asked for. A caller who asks for less than
    // 256 MB silently gets less, down to WiredTiger's own 1 MB floor, which is why the range
    // check above is this library's job rather than MongoDB's.
    mongo::wiredTigerGlobalOptions.cacheSizeGB = options.cacheSizeMB / 1024.0;
    mongo::wiredTigerGlobalOptions.engineConfig = options.wiredTigerJournalConfig();

    // The second WiredTiger instance, the scratch one query spilling writes into, keeps the
    // fixed 64 MB ceiling this engine has always given it. It is a separate arena whose
    // directory is wiped at every startup, it is already an order of magnitude below mongod's
    // 1000 MB default, and nothing measured says it is worth moving.
    mongo::gSpillWiredTigerCacheSizeMinMB = 64;
    mongo::gSpillWiredTigerCacheSizeMaxMB = 64;
}

}  // namespace embedded_mongodb
