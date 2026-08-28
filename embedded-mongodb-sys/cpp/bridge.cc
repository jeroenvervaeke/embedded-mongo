#include "embedded-mongodb-sys/src/ffi.rs.h"

#include <stdexcept>
#include <string>
#include <utility>

namespace embedded_mongodb {
namespace {

void throw_if_error(int status, char* error) {
    if (status == 0) {
        embedded_mongodb_free(error);
        return;
    }

    std::string message = error ? error : "unknown embedded MongoDB error";
    embedded_mongodb_free(error);
    throw std::runtime_error(message);
}

void forward_log(std::int32_t severity,
                 std::int32_t id,
                 const char* component,
                 std::size_t componentLen,
                 const char* context,
                 std::size_t contextLen,
                 const char* message,
                 std::size_t messageLen,
                 const char* record,
                 std::size_t recordLen) noexcept {
    emit_mongodb_log(severity,
                     id,
                     rust::Str(component, componentLen),
                     rust::Str(context, contextLen),
                     rust::Str(message, messageLen),
                     rust::Str(record, recordLen));
}

const std::string initializerError = [] {
    char* error = nullptr;
    const auto status = embedded_mongodb_initialize(&forward_log, &error);
    std::string message;
    if (status != 0) {
        message = error ? error : "failed to initialize embedded MongoDB";
    }
    embedded_mongodb_free(error);
    return message;
}();

}  // namespace

EmbeddedMongo::EmbeddedMongo(embedded_mongodb_handle* handle) noexcept : handle_(handle) {}

EmbeddedMongo::~EmbeddedMongo() {
    if (!handle_) {
        return;
    }

    char* error = nullptr;
    embedded_mongodb_close(std::exchange(handle_, nullptr), &error);
    embedded_mongodb_free(error);
}

rust::Vec<std::uint8_t> EmbeddedMongo::run_command(
    rust::Str database, rust::Slice<const std::uint8_t> command) const {
    if (!handle_) {
        throw std::runtime_error("embedded MongoDB client is closed");
    }

    embedded_mongodb_buffer response{};
    char* error = nullptr;
    const auto status = embedded_mongodb_run_command(handle_,
                                                     database.data(),
                                                     database.size(),
                                                     command.data(),
                                                     command.size(),
                                                     &response,
                                                     &error);
    throw_if_error(status, error);

    std::unique_ptr<void, decltype(&embedded_mongodb_free)> owner(
        response.data, &embedded_mongodb_free);
    rust::Vec<std::uint8_t> result;
    result.reserve(response.len);
    for (std::size_t index = 0; index < response.len; ++index) {
        result.push_back(response.data[index]);
    }
    return result;
}

void EmbeddedMongo::close() {
    if (!handle_) {
        return;
    }

    char* error = nullptr;
    const auto status =
        embedded_mongodb_close(std::exchange(handle_, nullptr), &error);
    throw_if_error(status, error);
}

std::unique_ptr<EmbeddedMongo> open(rust::Str path) {
    // A value-initialized NativeOpenOptions is zero in every field, which the engine reads as
    // "use your own defaults" -- so this stays the same open it always was, whatever the
    // library's defaults become.
    return open_with_options(path, NativeOpenOptions{});
}

std::unique_ptr<EmbeddedMongo> open_with_options(rust::Str path,
                                                 const NativeOpenOptions& options) {
    if (!initializerError.empty()) {
        throw std::runtime_error(initializerError);
    }

    // `size` is what tells the engine how much of this struct exists. Filling it in here
    // rather than in Rust keeps the two sides from having to agree on a number that changes
    // whenever a field is added.
    embedded_mongodb_open_options native{};
    native.size = sizeof(native);
    native.cache_size_mb = options.cache_size_mb;
    native.journal_file_max_kb = options.journal_file_max_kb;
    native.journal_prealloc = options.journal_prealloc;

    embedded_mongodb_handle* handle = nullptr;
    char* error = nullptr;
    const auto status = embedded_mongodb_open_with_options(
        path.data(), path.size(), &native, &handle, &error);
    throw_if_error(status, error);
    if (!handle) {
        throw std::runtime_error("embedded MongoDB returned a null client");
    }
    return std::unique_ptr<EmbeddedMongo>(new EmbeddedMongo(handle));
}

}  // namespace embedded_mongodb
