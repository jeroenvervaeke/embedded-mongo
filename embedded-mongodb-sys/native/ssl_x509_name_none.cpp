// `SSLX509Name::toString` for a build with no TLS stack.
//
// `ssl_peer_info.cpp` is compiled into this library (see BUILD.bazel) because the command path
// reaches `SSLPeerInfo::forSession`. Its `appendPeerInfoToVector` is the one caller of
// `SSLX509Name::toString`, whose definition lives in `ssl_manager.cpp` alongside the entire TLS
// manager, its OID tables and its RFC 2253 escaping -- none of which this library builds.
//
// With `--//bazel/config:ssl=False` there is no handshake and no peer certificate, so every
// `SSLX509Name` in this process is default-constructed and empty. An empty string is the
// faithful answer here rather than a stub, and returning one keeps a library that has no
// certificates from aborting the host process over a name it can never have.
//
// Release builds linked without this only because whole-program optimization removed the call
// before the linker looked for it. A debug build keeps the call and failed to load with
// `undefined symbol: mongo::SSLX509Name::toString`.

#include "mongo/util/net/ssl_types.h"

#include <string>

namespace mongo {

std::string SSLX509Name::toString() const {
    return {};
}

}  // namespace mongo
