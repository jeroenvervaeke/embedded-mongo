#pragma once

namespace embedded_mongodb {

/// Wraps the build's real version information so that `buildInfo` reports an "embedded"
/// module. Must run after `runGlobalInitializers`, which is when //src/mongo/util:version_impl
/// installs the implementation this decorates.
///
/// The `embeddedMongodb` command and the `embedded` section of `serverStatus` need no call:
/// both register themselves from this translation unit's static initializers, which run as
/// long as anything in it -- this function -- is referenced.
void installEmbeddedVersionInfo();

}  // namespace embedded_mongodb
