#include "embedded_mongodb_native.h"

#include "engine_options.h"
#include "engine_runtime.h"
#include "engine_startup.h"

#include "mongo/base/initializer.h"
#include "mongo/util/assert_util.h"

#include <cstdlib>
#include <cstring>
#include <exception>
#include <memory>
#include <stdexcept>
#include <string>
#include <string_view>
#include <utility>

// MongoDB's sharding initialization registers an initializer node under this name, and
// PrimaryOnlyServiceRegistry declares a dependency on it. This build links no sharding runtime,
// so nothing registers it and the initializer graph refuses to run with "depends on missing
// node". Standing in for it is enough: there is no sharding state to initialize.
MONGO_INITIALIZER_GENERAL(ShardingInitializationMongoDRegistry, (), ())
(mongo::InitializerContext*) {}

// mongod registers this node while storing its command-line options, in mongod_options_init.cpp,
// which belongs to the server binary rather than to any library here. SASL option storage
// declares a dependency on it, so the initializer graph needs the node to exist. The library
// previously got it from an empty stub inside AuthorizationManagerFactoryMock; that mock is gone,
// and there are no command-line options to store, so the stub belongs here instead.
MONGO_INITIALIZER_GENERAL(CoreOptions_Store, (), ())
(mongo::InitializerContext*) {}

namespace {

void setError(char** destination, std::string_view message) {
    if (!destination) {
        return;
    }

    auto* copy = static_cast<char*>(std::malloc(message.size() + 1));
    if (!copy) {
        return;
    }
    std::memcpy(copy, message.data(), message.size());
    copy[message.size()] = '\0';
    *destination = copy;
}

template <typename Function>
int translateErrors(char** error, Function&& function) noexcept {
    if (error) {
        *error = nullptr;
    }

    try {
        function();
        return 0;
    } catch (const mongo::DBException& exception) {
        setError(error, exception.toString());
    } catch (const std::exception& exception) {
        setError(error, exception.what());
    } catch (...) {
        setError(error, "unknown embedded MongoDB error");
    }
    return 1;
}

}  // namespace

struct embedded_mongodb_handle {
    embedded_mongodb_handle(std::string path,
                            const embedded_mongodb::ResolvedOptions& options)
        : runtime(std::make_shared<embedded_mongodb::Runtime>(std::move(path), options)) {}

    // Shared with every session so the Runtime *object* is never freed under one: a command on
    // a session whose handle has been closed then reads a null ServiceContext and fails
    // cleanly, rather than dereferencing freed memory. It does not keep the engine *running* --
    // `close` still tears the ServiceContext down -- so a session destroyed after that close
    // would still touch a dead service. Nothing here prevents that; the Rust layer does, by
    // holding an Arc on the runtime in every session so the handle cannot close while one lives.
    std::shared_ptr<embedded_mongodb::Runtime> runtime;
};

struct embedded_mongodb_session {
    explicit embedded_mongodb_session(std::shared_ptr<embedded_mongodb::Runtime> runtime)
        : session(std::move(runtime)) {}

    embedded_mongodb::Session session;
};

extern "C" {

int embedded_mongodb_initialize(embedded_mongodb_log_callback logCallback, char** error) noexcept {
    return translateErrors(error,
                           [logCallback] { embedded_mongodb::runInitializers(logCallback); });
}

int embedded_mongodb_open(const char* path,
                          std::size_t pathLen,
                          embedded_mongodb_handle** handle,
                          char** error) noexcept {
    // Deliberately delegating rather than duplicating: this entry point predates
    // embedded_mongodb_open_options and has to keep behaving like the library's defaults,
    // whatever those become.
    return embedded_mongodb_open_with_options(path, pathLen, nullptr, handle, error);
}

int embedded_mongodb_open_with_options(const char* path,
                                       std::size_t pathLen,
                                       const embedded_mongodb_open_options* options,
                                       embedded_mongodb_handle** handle,
                                       char** error) noexcept {
    return translateErrors(error, [&] {
        if (!path || !handle) {
            throw std::invalid_argument("path and handle are required");
        }
        *handle = nullptr;
        // Before the runtime is constructed, so that a rejected option is an error the caller
        // sees rather than a directory left half-opened.
        const auto resolved = embedded_mongodb::resolveOptions(options);
        *handle = new embedded_mongodb_handle(std::string(path, pathLen), resolved);
    });
}

int embedded_mongodb_session_open(embedded_mongodb_handle* handle,
                                  embedded_mongodb_session** session,
                                  char** error) noexcept {
    return translateErrors(error, [&] {
        if (!handle || !session) {
            throw std::invalid_argument("handle and session are required");
        }
        *session = nullptr;
        *session = new embedded_mongodb_session(handle->runtime);
    });
}

int embedded_mongodb_session_run_command(embedded_mongodb_session* session,
                                         const char* database,
                                         std::size_t databaseLen,
                                         const std::uint8_t* command,
                                         std::size_t commandLen,
                                         embedded_mongodb_buffer* response,
                                         char** error) noexcept {
    return translateErrors(error, [&] {
        if (!session || !database || !response) {
            throw std::invalid_argument("session, database, and response are required");
        }

        response->data = nullptr;
        response->len = 0;
        auto bytes = session->session.runCommand(
            std::string_view(database, databaseLen), command, commandLen);
        auto* copy = static_cast<std::uint8_t*>(std::malloc(bytes.size()));
        if (!copy) {
            throw std::bad_alloc();
        }
        std::memcpy(copy, bytes.data(), bytes.size());
        response->data = copy;
        response->len = bytes.size();
    });
}

int embedded_mongodb_session_close(embedded_mongodb_session* session, char** error) noexcept {
    return translateErrors(error, [&] {
        // Dropping the session releases its client and its share in the runtime. Nothing here
        // tears the engine down; the last handle does that.
        std::unique_ptr<embedded_mongodb_session> owner(session);
    });
}

int embedded_mongodb_close(embedded_mongodb_handle* handle, char** error) noexcept {
    return translateErrors(error, [&] {
        std::unique_ptr<embedded_mongodb_handle> owner(handle);
        if (owner && owner->runtime) {
            owner->runtime->close();
        }
    });
}

void embedded_mongodb_free(void* memory) noexcept {
    std::free(memory);
}

}
