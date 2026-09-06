#pragma once

#include "engine_options.h"

#include "mongo/db/client_strand.h"

#include <condition_variable>
#include <cstddef>
#include <cstdint>
#include <mutex>
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

    /// Takes a strand out of the pool, waiting for one when every strand is running a
    /// command. Pairs with `releaseStrand`; `runCommand` is the only caller of either --
    /// startup and shutdown use `_strands.front()` directly, before commands can arrive and
    /// after the caller's exclusive access says none are left.
    std::size_t acquireStrand();
    void releaseStrand(std::size_t index);

    mongo::ServiceContext* _serviceContext = nullptr;
    /// One strand per command that may run in parallel: each wraps its own `mongo::Client`,
    /// which is what a connection is to a server, so commands on different strands contend
    /// only where mongod's own sessions do -- in the lock manager and the storage engine.
    /// Sized once at open from `ResolvedOptions::commandStrands`.
    std::vector<mongo::ClientStrandPtr> _strands;
    std::mutex _poolMutex;
    std::condition_variable _strandReturned;
    /// Indices into `_strands` not currently bound to a command.
    std::vector<std::size_t> _freeStrands;
    bool _storageStarted = false;
    bool _indexBuildsStarted = false;
    bool _ownsActiveRuntime = false;
};

}  // namespace embedded_mongodb
