#include "embedded-mongodb/bridge.h"

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

const std::string initializerError = [] {
    char* error = nullptr;
    const auto status = embedded_mongodb_initialize(&error);
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
    if (!initializerError.empty()) {
        throw std::runtime_error(initializerError);
    }

    embedded_mongodb_handle* handle = nullptr;
    char* error = nullptr;
    const auto status =
        embedded_mongodb_open(path.data(), path.size(), &handle, &error);
    throw_if_error(status, error);
    if (!handle) {
        throw std::runtime_error("embedded MongoDB returned a null client");
    }
    return std::unique_ptr<EmbeddedMongo>(new EmbeddedMongo(handle));
}

}  // namespace embedded_mongodb
