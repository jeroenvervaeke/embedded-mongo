#include "engine_runtime.h"

#include "engine_recovery.h"
#include "engine_services.h"
#include "engine_startup.h"

#include "mongo/bson/bson_validate.h"
#include "mongo/db/dbdirectclient.h"
#include "mongo/db/index_builds/index_builds_coordinator.h"
#include "mongo/db/server_options.h"
#include "mongo/db/service_context.h"
#include "mongo/db/shard_role/lock_manager/d_concurrency.h"
#include "mongo/db/shard_role/lock_manager/lock_manager_defs.h"
#include "mongo/db/shard_role/shard_catalog/collection_catalog.h"
#include "mongo/db/shard_role/shard_catalog/collection_catalog_helper.h"
#include "mongo/db/shard_role/shard_catalog/collection_sharding_state.h"
#include "mongo/db/shard_role/shard_catalog/database_holder.h"
#include "mongo/db/shard_role/shard_catalog/database_sharding_state.h"
#include "mongo/db/storage/control/storage_control.h"
#include "mongo/db/storage/storage_engine_lock_file.h"
#include "mongo/db/storage/storage_options.h"
#include "mongo/db/topology/cluster_role.h"
#include "mongo/rpc/op_msg.h"
#include "mongo/util/assert_util.h"
#include "mongo/util/version/releases.h"

#include <exception>
#include <filesystem>
#include <memory>
#include <mutex>
#include <utility>

namespace embedded_mongodb {
namespace {

std::mutex runtimeMutex;
bool runtimeActive = false;

}  // namespace

Runtime::Runtime(std::string path, const ResolvedOptions& options) {
    {
        std::lock_guard lock(runtimeMutex);
        uassert(13180000,
                "only one embedded MongoDB runtime may be open per process",
                !runtimeActive);
        runtimeActive = true;
        _ownsActiveRuntime = true;
    }

    try {
        initialize(std::move(path), options);
    } catch (...) {
        cleanup(false);
        throw;
    }
}

Runtime::~Runtime() {
    cleanup(false);
}

std::vector<std::uint8_t> Runtime::runCommand(std::string_view database,
                                              const std::uint8_t* command,
                                              std::size_t commandLen) {
    uassert(13180001, "embedded MongoDB runtime is closed", _serviceContext);
    uassert(13180002, "invalid database name", mongo::DatabaseName::validDBName(database));
    uassert(13180003, "BSON command is empty", command && commandLen);
    uassertStatusOK(mongo::validateBSON(reinterpret_cast<const char*>(command), commandLen));

    mongo::BSONObj commandObject(reinterpret_cast<const char*>(command));
    uassert(13180004,
            "BSON command contains trailing bytes",
            static_cast<std::size_t>(commandObject.objsize()) == commandLen);

    auto clientGuard = _strand->bind();
    auto opCtx = _serviceContext->makeOperationContext(clientGuard.get());
    mongo::DBDirectClient client(opCtx.get());
    auto request = mongo::OpMsgRequestBuilder::create(
        mongo::auth::ValidatedTenancyScope::kNotRequired,
        mongo::DatabaseName::createDatabaseName_forTest(boost::none, database),
        commandObject);
    auto reply = client.runCommand(std::move(request));
    const auto& response = reply->getCommandReply();

    const auto* begin = reinterpret_cast<const std::uint8_t*>(response.objdata());
    return {begin, begin + response.objsize()};
}

void Runtime::close() {
    cleanup(true);
}

void Runtime::initialize(std::string path, const ResolvedOptions& options) {
    runInitializers();

    uassert(13180005, "database directory cannot be empty", !path.empty());
    auto dbPath = std::filesystem::absolute(std::filesystem::path(std::move(path)));
    std::filesystem::create_directories(dbPath);

    mongo::serverGlobalParams.clusterRole = mongo::ClusterRole::ShardServer;
    mongo::setGlobalServiceContext(mongo::ServiceContext::make());
    _serviceContext = mongo::getGlobalServiceContext();

    installProcessServices(_serviceContext);

    _strand = mongo::ClientStrand::make(
        _serviceContext->getService()->makeClient("embedded-mongodb", nullptr));
    auto clientGuard = _strand->bind();

    mongo::storageGlobalParams.dbpath = dbPath.string();
    mongo::storageGlobalParams.engine = "wiredTiger";
    mongo::storageGlobalParams.engineSetByUser = true;
    mongo::storageGlobalParams.repair = false;
    applyOptions(options);
    mongo::serverGlobalParams.mutableFCV.setVersion(mongo::multiversion::GenericFCV::kLatest);

    installCatalogServices(_serviceContext);
    _indexBuildsStarted = true;

    mongo::StorageEngineLockFile::create(_serviceContext, mongo::storageGlobalParams.dbpath);
    auto& lockFile = mongo::StorageEngineLockFile::get(_serviceContext);
    if (lockFile) {
        uassertStatusOK(lockFile->writePid());
    }

    // The shutdown state is what tells the reconciliation in recoverCatalog whether the
    // internal idents left in the directory are the remains of a resumable index build or
    // rubbish from a process that was killed. mongod threads the same value into its startup
    // recovery.
    const auto lastShutdownState = mongo::catalog::startUpStorageEngineAndCollectionCatalog(
        _serviceContext, clientGuard.get(), mongo::StorageEngineInitFlags{});
    _storageStarted = true;
    mongo::StorageControl::startStorageControls(_serviceContext);

    recoverCatalog(_serviceContext, clientGuard.get(), lastShutdownState);

    _serviceContext->getStorageEngine()->notifyStorageStartupRecoveryComplete();
    _serviceContext->notifyStorageStartupRecoveryComplete();
}

void Runtime::cleanup(bool reportFailure) {
    std::exception_ptr failure;
    auto attempt = [&](auto&& action) {
        try {
            action();
        } catch (...) {
            if (!failure) {
                failure = std::current_exception();
            }
        }
    };

    if (_serviceContext) {
        if (_indexBuildsStarted && _strand) {
            _indexBuildsStarted = false;
            attempt([&] {
                auto clientGuard = _strand->bind();
                auto opCtx = _serviceContext->makeOperationContext(clientGuard.get());
                mongo::IndexBuildsCoordinator::get(_serviceContext)->shutdown(opCtx.get());
            });
        }

        attempt([&] {
            mongo::CollectionShardingStateFactory::clear(_serviceContext);
            mongo::DatabaseShardingStateFactory::clear(_serviceContext);
        });

        if (_storageStarted && _strand) {
            attempt([&] {
                auto clientGuard = _strand->bind();
                auto opCtx = _serviceContext->makeOperationContext(clientGuard.get());
                mongo::Lock::GlobalLock globalLock(opCtx.get(), mongo::MODE_X);
                mongo::DatabaseHolder::get(opCtx.get())->closeAll(opCtx.get());
            });
            _storageStarted = false;
            attempt([&] {
                mongo::catalog::shutDownCollectionCatalogAndGlobalStorageEngineCleanly(
                    _serviceContext, false);
            });
        }

        attempt([&] {
            auto& lockFile = mongo::StorageEngineLockFile::get(_serviceContext);
            if (lockFile) {
                lockFile->clearPidAndUnlock();
                lockFile = boost::none;
            }
        });

        _strand.reset();
        mongo::setGlobalServiceContext({});
        _serviceContext = nullptr;
    }

    if (_ownsActiveRuntime) {
        std::lock_guard lock(runtimeMutex);
        runtimeActive = false;
        _ownsActiveRuntime = false;
    }

    if (reportFailure && failure) {
        std::rethrow_exception(failure);
    }
}

}  // namespace embedded_mongodb
