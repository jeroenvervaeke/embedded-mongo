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
    /// Every session opened on this runtime must be gone before this runs: a session that
    /// outlived it would hold a client of a service that no longer exists, and destroying that
    /// client would reach into freed memory. The safe Rust layer drops its sessions first.
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
/// A session borrows the runtime that owns its client: destroying the strand deregisters that
/// client from the runtime's Service, so the runtime must outlive every session opened on it.
/// Nothing here enforces that, deliberately -- the safe Rust layer does, by giving each session
/// an `Arc` on the runtime handle, which is what keeps the handle from closing while a session
/// lives. Ownership rules are policy, and policy lives in the language that checks it.
class Session {
public:
    explicit Session(Runtime& runtime);

    Session(const Session&) = delete;
    Session& operator=(const Session&) = delete;

    std::vector<std::uint8_t> runCommand(std::string_view database,
                                         const std::uint8_t* command,
                                         std::size_t commandLen);

    /// Interrupts the command this session is running, if it is running one.
    ///
    /// Callable from any thread, and the only method here that is: it takes the session's
    /// Client lock, which is what MongoDB's own `killOp` takes, and reads and kills the
    /// operation under it. A session between commands has no operation attached and this does
    /// nothing. Interruption is cooperative -- the operation stops at its next interrupt check
    /// -- so this asks, promptly, rather than tears anything down.
    ///
    /// Const because it changes nothing here: the state it touches belongs to the Client.
    void kill() const;

private:
    Runtime& _runtime;
    mongo::ClientStrandPtr _strand;
};

}  // namespace embedded_mongodb
