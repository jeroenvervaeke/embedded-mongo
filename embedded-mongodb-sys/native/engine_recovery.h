#pragma once

#include "mongo/db/storage/storage_engine.h"

namespace mongo {
class Client;
class ServiceContext;
}  // namespace mongo

namespace embedded_mongodb {

/// The part of mongod's startup recovery a non-replicated engine still needs, run under one
/// global write lock immediately after the storage engine comes up. See engine_recovery.cpp
/// for which parts those are and why the rest is left out.
void recoverCatalog(mongo::ServiceContext* serviceContext,
                    mongo::Client* client,
                    mongo::StorageEngine::LastShutdownState lastShutdownState);

}  // namespace embedded_mongodb
