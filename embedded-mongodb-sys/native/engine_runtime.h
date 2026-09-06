#pragma once

#include "engine_options.h"

#include "mongo/db/client_strand.h"

#include <cstddef>
#include <cstdint>
#include <memory>
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
///
/// It runs no commands itself. A command needs a `mongo::Client` -- what a connection is to a
/// server -- and those are handed out as [`Session`]s, each with its own client, so that two
/// sessions run in parallel and contend only where mongod's own connections do. The one strand
/// the runtime keeps is for its own lifecycle -- startup recovery and shutdown -- which are
/// single-threaded by construction and never overlap a command.
class Runtime {
public:
    Runtime(std::string path, const ResolvedOptions& options);
    ~Runtime();

    Runtime(const Runtime&) = delete;
    Runtime& operator=(const Runtime&) = delete;

    /// The service every session makes its client from, or null once the runtime is closed.
    /// A session reads this before it binds anything, so a command on a closed runtime is a
    /// named error rather than a use of a torn-down engine.
    mongo::ServiceContext* serviceContext() const { return _serviceContext; }

    /// Shuts the engine down and reports what failed on the way. The destructor does the same
    /// work silently, so a caller who does not want to hear about it can simply drop this.
    ///
    /// Every session opened on this runtime must be gone before this runs. Sessions hold a
    /// shared reference to the runtime, so the object cannot be freed under them; but this
    /// tears the engine down, and a session that outlived it would hold a client of a service
    /// that no longer exists. The safe Rust layer drops its sessions before it closes.
    void close();

private:
    void initialize(std::string path, const ResolvedOptions& options);
    void cleanup(bool reportFailure);

    mongo::ServiceContext* _serviceContext = nullptr;
    /// The runtime's own strand, for startup recovery and shutdown only -- never a command.
    /// Commands run on a session's strand instead.
    mongo::ClientStrandPtr _lifecycleStrand;
    bool _storageStarted = false;
    bool _indexBuildsStarted = false;
    bool _ownsActiveRuntime = false;
};

/// One `mongo::Client` on an open [`Runtime`] -- the embedded equivalent of a connection.
///
/// A session binds its own strand for the length of each command, so N sessions run N commands
/// at once with no coordination of their own: the parallelism, and the serialization of one
/// session's successive commands, are both the strand's doing. Deciding how many sessions to
/// open, and handing them out, is the caller's job -- there is no pool here, because a pool is
/// policy and belongs in the Rust layer that has a checked language to write it in.
///
/// A session keeps its runtime alive by holding a shared reference to it, so it can never bind
/// a client of a runtime that has been freed. It must still be destroyed before the runtime is
/// *closed*: see [`Runtime::close`].
class Session {
public:
    explicit Session(std::shared_ptr<Runtime> runtime);

    Session(const Session&) = delete;
    Session& operator=(const Session&) = delete;

    std::vector<std::uint8_t> runCommand(std::string_view database,
                                         const std::uint8_t* command,
                                         std::size_t commandLen);

private:
    // Order is load-bearing: members destroy in reverse declaration order, so `_strand` (the
    // Client) is torn down before this session's `_runtime` reference is released. Destroying a
    // Client deregisters it from its Service, so the Service must still be alive at that point;
    // declaring `_runtime` first keeps it alive across the strand's destruction. Do not reorder.
    std::shared_ptr<Runtime> _runtime;
    mongo::ClientStrandPtr _strand;
};

}  // namespace embedded_mongodb
