#pragma once

#include "engine_options.h"

#include "mongo/db/client_strand.h"

#include <cstddef>
#include <cstdint>
#include <string>
#include <string_view>
#include <vector>

namespace mongo {
class ServiceContext;
}  // namespace mongo

namespace embedded_mongodb {

/// One open database directory, and everything the engine keeps alive for it.
///
/// At most one may exist per process: MongoDB reaches its storage engine, its catalog and its
/// options through process-wide globals, so a second directory would be opened over the first.
/// The constructor throws rather than allowing that.
class Runtime {
public:
    Runtime(std::string path, const ResolvedOptions& options);
    ~Runtime();

    Runtime(const Runtime&) = delete;
    Runtime& operator=(const Runtime&) = delete;

    std::vector<std::uint8_t> runCommand(std::string_view database,
                                         const std::uint8_t* command,
                                         std::size_t commandLen);

    /// Shuts the engine down and reports what failed on the way. The destructor does the same
    /// work silently, so a caller who does not want to hear about it can simply drop this.
    void close();

private:
    void initialize(std::string path, const ResolvedOptions& options);
    void cleanup(bool reportFailure);

    mongo::ServiceContext* _serviceContext = nullptr;
    mongo::ClientStrandPtr _strand;
    bool _storageStarted = false;
    bool _indexBuildsStarted = false;
    bool _ownsActiveRuntime = false;
};

}  // namespace embedded_mongodb
