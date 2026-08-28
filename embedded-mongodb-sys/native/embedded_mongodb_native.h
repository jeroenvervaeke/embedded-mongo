#pragma once

#include <cstddef>
#include <cstdint>

#if defined(__GNUC__)
#define EMBEDDED_MONGODB_API __attribute__((visibility("default")))
#else
#define EMBEDDED_MONGODB_API
#endif

extern "C" {

struct embedded_mongodb_handle;

struct embedded_mongodb_buffer {
    std::uint8_t* data;
    std::size_t len;
};

/// Whether WiredTiger keeps a spare journal file ready ahead of the one it is writing.
///
/// Zero is not a state: a partially filled options struct has to be rejected rather than
/// silently read as one of the two answers.
enum embedded_mongodb_journal_prealloc {
    EMBEDDED_MONGODB_JOURNAL_PREALLOC_DEFAULT = 0,
    EMBEDDED_MONGODB_JOURNAL_PREALLOC_ENABLED = 1,
    EMBEDDED_MONGODB_JOURNAL_PREALLOC_DISABLED = 2,
};

/// The storage limits that can only be chosen while WiredTiger is being opened.
///
/// Deliberately narrow. MongoDB's other sizing knobs are server parameters, reachable from a
/// client through `setParameter` after the engine is up, and belong there rather than in a C
/// ABI that has to be rebuilt and re-released to grow a field.
///
/// Zero means "whatever this library defaults to" in every field, which is what makes the
/// struct safe to memset and safe to grow -- every field except `size`, which must be set and
/// is rejected when it is not. `size` is `sizeof` as the *caller* compiled it:
/// a field added after the caller was built is absent, reads as zero, and takes the library
/// default; a field a newer caller sets that this library does not know about is ignored.
/// One entry point therefore survives every option added later, and the five entry points
/// that predate this struct keep their signatures for callers that never ask for any of it.
struct embedded_mongodb_open_options {
    std::size_t size;
    /// WiredTiger cache ceiling, in MiB. A ceiling, not a reservation: the engine grows into
    /// it only as pages are read.
    std::uint32_t cache_size_mb;
    /// Size of one journal file, in KiB. Every journal file is allocated at this size the
    /// moment it is created, so this is the floor on what an empty database costs on disk.
    std::uint32_t journal_file_max_kb;
    /// One of embedded_mongodb_journal_prealloc.
    std::uint32_t journal_prealloc;
};

typedef void (*embedded_mongodb_log_callback)(std::int32_t severity,
                                               std::int32_t id,
                                               const char* component,
                                               std::size_t component_len,
                                               const char* context,
                                               std::size_t context_len,
                                               const char* message,
                                               std::size_t message_len,
                                               const char* record,
                                               std::size_t record_len) noexcept;

EMBEDDED_MONGODB_API int embedded_mongodb_initialize(
    embedded_mongodb_log_callback log_callback, char** error) noexcept;

EMBEDDED_MONGODB_API int embedded_mongodb_open(const char* path,
                                                std::size_t path_len,
                                                embedded_mongodb_handle** handle,
                                                char** error) noexcept;

/// `embedded_mongodb_open` with the limits above overridden. A null `options` is exactly
/// `embedded_mongodb_open`.
EMBEDDED_MONGODB_API int embedded_mongodb_open_with_options(
    const char* path,
    std::size_t path_len,
    const embedded_mongodb_open_options* options,
    embedded_mongodb_handle** handle,
    char** error) noexcept;

EMBEDDED_MONGODB_API int embedded_mongodb_run_command(embedded_mongodb_handle* handle,
                                                       const char* database,
                                                       std::size_t database_len,
                                                       const std::uint8_t* command,
                                                       std::size_t command_len,
                                                       embedded_mongodb_buffer* response,
                                                       char** error) noexcept;

EMBEDDED_MONGODB_API int embedded_mongodb_close(embedded_mongodb_handle* handle,
                                                 char** error) noexcept;

EMBEDDED_MONGODB_API void embedded_mongodb_free(void* memory) noexcept;

}
