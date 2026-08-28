#include "engine_recovery.h"

#include "mongo/db/service_context.h"
#include "mongo/db/shard_role/lock_manager/d_concurrency.h"
#include "mongo/db/shard_role/lock_manager/lock_manager_defs.h"
#include "mongo/db/shard_role/shard_catalog/catalog_repair.h"
#include "mongo/db/shard_role/shard_catalog/collection_catalog.h"
#include "mongo/db/shard_role/shard_catalog/database_holder.h"
#include "mongo/db/storage/storage_options.h"
#include "mongo/util/assert_util.h"

namespace embedded_mongodb {

// Starting the storage engine fills the CollectionCatalog and nothing else.
// catalog::initCollectionObject stops at Collection::Factory::make, so a database
// restored from disk is absent from DatabaseHolder and each of its collections carries
// an uninitialized, empty in-memory IndexCatalog.
//
// mongod closes both gaps in one call, during startup recovery:
// startup_recovery::repairAndRecoverDatabases -> openDatabases -> DatabaseHolder::openDb,
// which registers the Database and runs DatabaseImpl::init -> CollectionImpl::init ->
// IndexCatalog::init over every collection in it. Nothing else in the server does either.
//
// Without it, a reopened directory: returns an empty listCollections batch (the command
// gates on DatabaseHolder::dbExists); hides every index from the query planner, collStats
// and validate; stops maintaining index keys on write, including _id_; and aborts the
// host process on any command that acquires a collection by UUID -- createIndexes and
// dropIndexes both do -- via the "Database for <ns> disappeared after successfully
// resolving <uuid>" invariant in AutoGetCollection.
//
// catalog_repair::reconcileCatalogAndIdents has to run first, in the same global lock and
// in mongod's order (startup_recovery.cpp:1020 then :1032). A createIndexes this engine
// did not finish -- this one builds single-phase, so its unfinished indexes carry no
// buildUUID -- leaves a non-ready index in the durable catalog, and IndexCatalog::init
// asserts that every non-ready index it meets is a two-phase build
// (index_catalog_impl.cpp:318). Reconciliation is what drops those entries, along with
// idents no catalog entry references any more. Without it, opening a directory whose
// index build was interrupted -- process death on Android, i.e. routine -- aborts here.
//
// Two-phase index builds are deliberately not restarted afterwards, as mongod skips for a
// replica set member started standalone: restarting one starts a background thread that
// waits for a replicated commit that a non-replicated engine will never produce. This
// engine cannot create such a build in the first place.
//
// The rest of mongod's startup recovery -- repair, offline validation, FCV document
// creation -- does not apply here, and pulling it in would dereference a replication
// StorageInterface this runtime never installs.
void recoverCatalog(mongo::ServiceContext* serviceContext,
                    mongo::Client* client,
                    mongo::StorageEngine::LastShutdownState lastShutdownState) {
    auto opCtx = serviceContext->makeOperationContext(client);
    mongo::Lock::GlobalWrite globalLock(opCtx.get());

    auto* storageEngine = serviceContext->getStorageEngine();
    uassertStatusOK(mongo::catalog_repair::reconcileCatalogAndIdents(
        opCtx.get(),
        storageEngine,
        storageEngine->getStableTimestamp(),
        lastShutdownState,
        mongo::storageGlobalParams.repair));

    auto* databaseHolder = mongo::DatabaseHolder::get(opCtx.get());
    for (const auto& dbName : mongo::catalog::listDatabases()) {
        databaseHolder->openDb(opCtx.get(), dbName);
    }
}

}  // namespace embedded_mongodb
