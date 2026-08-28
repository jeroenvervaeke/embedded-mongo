#pragma once

#include "embedded_mongodb_native.h"
#include "rust/cxx.h"

#include <cstdint>
#include <memory>

namespace embedded_mongodb {

struct NativeOpenOptions;

class EmbeddedMongo {
public:
    ~EmbeddedMongo();

    EmbeddedMongo(const EmbeddedMongo&) = delete;
    EmbeddedMongo& operator=(const EmbeddedMongo&) = delete;

    rust::Vec<std::uint8_t> run_command(
        rust::Str database, rust::Slice<const std::uint8_t> command) const;
    void close();

private:
    friend std::unique_ptr<EmbeddedMongo> open_with_options(rust::Str path,
                                                            const NativeOpenOptions& options);

    explicit EmbeddedMongo(embedded_mongodb_handle* handle) noexcept;

    embedded_mongodb_handle* handle_;
};

std::unique_ptr<EmbeddedMongo> open(rust::Str path);

/// `open` with the storage limits in `options` applied. Every zero field there is the engine's
/// own default, so an all-zero struct opens exactly as `open` does.
std::unique_ptr<EmbeddedMongo> open_with_options(rust::Str path,
                                                 const NativeOpenOptions& options);

}  // namespace embedded_mongodb
