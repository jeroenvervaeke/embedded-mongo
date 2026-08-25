#include "embedded_mongodb_native.h"

#include "mongo/base/initializer.h"
#include "mongo/bson/bson_validate.h"
#include "mongo/db/auth/authorization_manager.h"
#include "mongo/db/auth/authorization_manager_factory_impl.h"
#include "mongo/db/client_strand.h"
#include "mongo/db/commands.h"
#include "mongo/db/dbdirectclient.h"
#include "mongo/db/index_builds/index_builds_coordinator.h"
#include "mongo/db/index_builds/index_builds_coordinator_mongod.h"
#include "mongo/db/op_observer/op_observer_registry.h"
#include "mongo/db/repl/repl_settings.h"
#include "mongo/db/repl/replication_coordinator.h"
#include "mongo/db/repl/replication_coordinator_mock.h"
#include "mongo/db/server_options.h"
#include "mongo/db/service_context.h"
#include "mongo/db/service_entry_point_shard_role.h"
#include "mongo/db/shard_role/lock_manager/d_concurrency.h"
#include "mongo/db/shard_role/lock_manager/lock_manager_defs.h"
#include "mongo/db/shard_role/shard_catalog/collection.h"
#include "mongo/db/shard_role/shard_catalog/collection_catalog_helper.h"
#include "mongo/db/shard_role/shard_catalog/collection_impl.h"
#include "mongo/db/shard_role/shard_catalog/collection_metadata.h"
#include "mongo/db/shard_role/shard_catalog/collection_sharding_state.h"
#include "mongo/db/shard_role/shard_catalog/database_holder.h"
#include "mongo/db/shard_role/shard_catalog/database_holder_impl.h"
#include "mongo/db/shard_role/shard_catalog/database_sharding_state.h"
#include "mongo/db/shard_role/shard_catalog/scoped_collection_metadata.h"
#include "mongo/db/storage/control/storage_control.h"
#include "mongo/db/storage/storage_engine.h"
#include "mongo/db/storage/storage_engine_lock_file.h"
#include "mongo/db/storage/storage_options.h"
#include "mongo/db/storage/wiredtiger/wiredtiger_global_options.h"
#include "mongo/db/storage/wiredtiger/wiredtiger_global_options_gen.h"
#include "mongo/db/topology/cluster_role.h"
#include "mongo/db/topology/sharding_state.h"
#include "mongo/db/wire_version.h"
#include "mongo/logv2/attributes.h"
#include "mongo/logv2/component_settings_filter.h"
#include "mongo/logv2/json_formatter.h"
#include "mongo/logv2/log_domain_global.h"
#include "mongo/logv2/log_manager.h"
#include "mongo/logv2/log_severity.h"
#include "mongo/rpc/op_msg.h"
#include "mongo/scripting/dbdirectclient_factory.h"
#include "mongo/util/assert_util.h"
#include "mongo/util/periodic_runner_factory.h"
#include "mongo/util/version/releases.h"

#include <cstdlib>
#include <cstring>
#include <exception>
#include <filesystem>
#include <memory>
#include <mutex>
#include <stdexcept>
#include <string>
#include <string_view>
#include <utility>
#include <vector>

#include <boost/log/attributes/value_extraction.hpp>
#include <boost/log/core/core.hpp>
#include <boost/log/sinks/basic_sink_backend.hpp>
#include <boost/log/sinks/unlocked_frontend.hpp>
#include <boost/smart_ptr/make_shared_object.hpp>

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

std::mutex runtimeMutex;
bool runtimeActive = false;
std::once_flag initializersOnce;

// The shard role asks every collection and database access for its sharding state. MongoDB's
// implementations of that live in the sharding runtime, which this library does not link: an
// embedded engine is a single node, every collection is untracked, and it owns all of its own
// data. These say exactly that, which is what lets db/s, mongo/s and the routing layer be cut
// from the build.
//
// A default-constructed CollectionMetadata is untracked -- no routing table, no shard key -- so
// one instance answers every description and ownership-filter query.

class UnshardedMetadata final : public mongo::ScopedCollectionDescription::Impl {
public:
    const mongo::CollectionMetadata& get() override {
        return _metadata;
    }

private:
    mongo::CollectionMetadata _metadata;
};

std::shared_ptr<UnshardedMetadata> unshardedMetadata() {
    static const auto instance = std::make_shared<UnshardedMetadata>();
    return instance;
}

class StandaloneCollectionShardingState final : public mongo::CollectionShardingState {
public:
    mongo::ScopedCollectionDescription getCollectionDescription(
        mongo::OperationContext*) const override {
        return mongo::ScopedCollectionDescription(unshardedMetadata());
    }

    mongo::ScopedCollectionDescription getCollectionDescription(mongo::OperationContext*,
                                                                bool) const override {
        return mongo::ScopedCollectionDescription(unshardedMetadata());
    }

    mongo::ScopedCollectionFilter getOwnershipFilter(mongo::OperationContext*,
                                                      OrphanCleanupPolicy,
                                                      bool) const override {
        return mongo::ScopedCollectionFilter(unshardedMetadata());
    }

    mongo::ScopedCollectionFilter getOwnershipFilter(mongo::OperationContext*,
                                                      OrphanCleanupPolicy,
                                                      const mongo::ShardVersion&) const override {
        return mongo::ScopedCollectionFilter(unshardedMetadata());
    }

    // Nothing here is versioned, so no operation can arrive with a stale version.
    void checkShardVersionOrThrow(mongo::OperationContext*) const override {}
    void checkShardVersionOrThrow(mongo::OperationContext*,
                                  const mongo::ShardVersion&) const override {}
    void appendShardVersion(mongo::BSONObjBuilder*) const override {}
};

class NoStaleCollectionMetadata final : public mongo::StaleShardCollectionMetadataHandler {
public:
    boost::optional<mongo::ChunkVersion> handleStaleShardVersionException(
        mongo::OperationContext*, const mongo::StaleConfigInfo&) const override {
        return boost::none;
    }
};

class StandaloneCollectionShardingStateFactory final
    : public mongo::CollectionShardingStateFactory {
public:
    std::unique_ptr<mongo::CollectionShardingState> make(const mongo::NamespaceString&) override {
        return std::make_unique<StandaloneCollectionShardingState>();
    }

    const mongo::StaleShardCollectionMetadataHandler& getStaleShardExceptionHandler()
        const override {
        return _handler;
    }

private:
    NoStaleCollectionMetadata _handler;
};

class StandaloneDatabaseShardingState final : public mongo::DatabaseShardingState {
public:
    void checkDbVersionOrThrow(mongo::OperationContext*) const override {}
    void checkDbVersionOrThrow(mongo::OperationContext*,
                               const mongo::DatabaseVersion&) const override {}
    void assertIsPrimaryShardForDb(mongo::OperationContext*) const override {}
    bool isMovePrimaryInProgress() const override {
        return false;
    }
};

class NoStaleDatabaseMetadata final : public mongo::StaleShardDatabaseMetadataHandler {
public:
    boost::optional<mongo::DatabaseVersion> handleStaleDatabaseVersionException(
        mongo::OperationContext*, const mongo::StaleDbRoutingVersion&) const override {
        return boost::none;
    }
};

class StandaloneDatabaseShardingStateFactory final : public mongo::DatabaseShardingStateFactory {
public:
    std::unique_ptr<mongo::DatabaseShardingState> make(const mongo::DatabaseName&) override {
        return std::make_unique<StandaloneDatabaseShardingState>();
    }

    const mongo::StaleShardDatabaseMetadataHandler& getStaleShardExceptionHandler() const override {
        return _handler;
    }

private:
    NoStaleDatabaseMetadata _handler;
};

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

class TracingLogBackend
    : public boost::log::sinks::
          basic_formatted_sink_backend<char, boost::log::sinks::concurrent_feeding> {
public:
    explicit TracingLogBackend(embedded_mongodb_log_callback callback) : _callback(callback) {}

    void consume(boost::log::record_view const& record, string_type const& formattedRecord) {
        using boost::log::extract;
        using namespace mongo::logv2;

        const auto severity = extract<LogSeverity>(attributes::severity(), record).get().toInt();
        const auto id = extract<std::int32_t>(attributes::id(), record).get();
        const auto component =
            extract<LogComponent>(attributes::component(), record).get().getNameForLog();
        const auto context = extract<std::string_view>(attributes::threadName(), record).get();
        const auto message = extract<std::string_view>(attributes::message(), record).get();
        const auto recordSize =
            formattedRecord.ends_with('\n') ? formattedRecord.size() - 1 : formattedRecord.size();
        _callback(severity,
                  id,
                  component.data(),
                  component.size(),
                  context.data(),
                  context.size(),
                  message.data(),
                  message.size(),
                  formattedRecord.data(),
                  recordSize);
    }

private:
    embedded_mongodb_log_callback _callback;
};

void runInitializers(embedded_mongodb_log_callback logCallback = nullptr) {
    std::call_once(initializersOnce, [logCallback] {
        auto& logManager = mongo::logv2::LogManager::global();
        mongo::logv2::LogDomainGlobal::ConfigurationOptions config;
        config.makeDisabled();
        uassertStatusOK(logManager.getGlobalDomainInternal().configure(config));
        if (logCallback) {
            auto sink =
                boost::make_shared<boost::log::sinks::unlocked_sink<TracingLogBackend>>(
                    boost::make_shared<TracingLogBackend>(logCallback));
            sink->set_filter(mongo::logv2::ComponentSettingsFilter(
                logManager.getGlobalDomain(), logManager.getGlobalSettings()));
            sink->set_formatter(mongo::logv2::JSONFormatter());
            boost::log::core::get()->add_sink(sink);
        }
        uassertStatusOK(mongo::runGlobalInitializers(std::vector<std::string>{}));
        mongo::getCommandRegistry(mongo::ClusterRole::ShardServer);
    });
}

class Runtime {
public:
    explicit Runtime(std::string path) {
        {
            std::lock_guard lock(runtimeMutex);
            uassert(13180000,
                    "only one embedded MongoDB runtime may be open per process",
                    !runtimeActive);
            runtimeActive = true;
            _ownsActiveRuntime = true;
        }

        try {
            initialize(std::move(path));
        } catch (...) {
            cleanup(false);
            throw;
        }
    }

    ~Runtime() {
        cleanup(false);
    }

    Runtime(const Runtime&) = delete;
    Runtime& operator=(const Runtime&) = delete;

    std::vector<std::uint8_t> runCommand(std::string_view database,
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

    void close() {
        cleanup(true);
    }

private:
    void initialize(std::string path) {
        runInitializers();

        uassert(13180005, "database directory cannot be empty", !path.empty());
        auto dbPath = std::filesystem::absolute(std::filesystem::path(std::move(path)));
        std::filesystem::create_directories(dbPath);

        mongo::serverGlobalParams.clusterRole = mongo::ClusterRole::ShardServer;
        mongo::setGlobalServiceContext(mongo::ServiceContext::make());
        _serviceContext = mongo::getGlobalServiceContext();

        mongo::WireSpec::Specification wireSpec;
        wireSpec.isInternalClient = true;
        mongo::WireSpec::getWireSpec(_serviceContext).initialize(std::move(wireSpec));

        auto authFactory = std::make_unique<mongo::AuthorizationManagerFactoryImpl>();
        mongo::AuthorizationManager::set(
            _serviceContext->getService(),
            authFactory->createShard(_serviceContext->getService()));
        mongo::AuthorizationManager::get(_serviceContext->getService())->setAuthEnabled(false);

        _serviceContext->getService()->setServiceEntryPoint(
            std::make_unique<mongo::ServiceEntryPointShardRole>());
        _serviceContext->setOpObserver(std::make_unique<mongo::OpObserverRegistry>());
        _serviceContext->setPeriodicRunner(mongo::makePeriodicRunner(_serviceContext));

        mongo::DBDirectClientFactory::get(_serviceContext)
            .registerImplementation([](mongo::OperationContext* opCtx) {
                return std::make_unique<mongo::DBDirectClient>(opCtx);
            });

        _strand = mongo::ClientStrand::make(
            _serviceContext->getService()->makeClient("embedded-mongodb", nullptr));
        auto clientGuard = _strand->bind();

        mongo::storageGlobalParams.dbpath = dbPath.string();
        mongo::storageGlobalParams.engine = "wiredTiger";
        mongo::storageGlobalParams.engineSetByUser = true;
        mongo::storageGlobalParams.repair = false;
        // ponytail: fixed prototype limits; expose cache options when workloads need tuning.
        mongo::wiredTigerGlobalOptions.cacheSizeGB = 0.25;
        mongo::gSpillWiredTigerCacheSizeMinMB = 64;
        mongo::gSpillWiredTigerCacheSizeMaxMB = 64;
        mongo::serverGlobalParams.mutableFCV.setVersion(mongo::multiversion::GenericFCV::kLatest);

        mongo::ShardingState::create(_serviceContext);
        mongo::CollectionShardingStateFactory::set(
            _serviceContext, std::make_unique<StandaloneCollectionShardingStateFactory>());
        mongo::DatabaseShardingStateFactory::set(
            _serviceContext, std::make_unique<StandaloneDatabaseShardingStateFactory>());
        mongo::DatabaseHolder::set(_serviceContext,
                                   std::make_unique<mongo::DatabaseHolderImpl>());
        mongo::Collection::Factory::set(_serviceContext,
                                        std::make_unique<mongo::CollectionImplFactory>());

        auto replCoordinator = std::make_unique<mongo::repl::ReplicationCoordinatorMock>(
            _serviceContext, mongo::repl::ReplSettings());
        uassertStatusOK(
            replCoordinator->setFollowerMode(mongo::repl::MemberState::RS_PRIMARY));
        mongo::repl::ReplicationCoordinator::set(_serviceContext,
                                                  std::move(replCoordinator));
        mongo::IndexBuildsCoordinator::set(
            _serviceContext, std::make_unique<mongo::IndexBuildsCoordinatorMongod>());
        _indexBuildsStarted = true;

        mongo::StorageEngineLockFile::create(_serviceContext, mongo::storageGlobalParams.dbpath);
        auto& lockFile = mongo::StorageEngineLockFile::get(_serviceContext);
        if (lockFile) {
            uassertStatusOK(lockFile->writePid());
        }

        mongo::catalog::startUpStorageEngineAndCollectionCatalog(
            _serviceContext, clientGuard.get(), mongo::StorageEngineInitFlags{});
        _storageStarted = true;
        mongo::StorageControl::startStorageControls(_serviceContext);

        _serviceContext->getStorageEngine()->notifyStorageStartupRecoveryComplete();
        _serviceContext->notifyStorageStartupRecoveryComplete();
    }

    void cleanup(bool reportFailure) {
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
                    mongo::IndexBuildsCoordinator::get(_serviceContext)
                        ->shutdown(opCtx.get());
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

    mongo::ServiceContext* _serviceContext = nullptr;
    mongo::ClientStrandPtr _strand;
    bool _storageStarted = false;
    bool _indexBuildsStarted = false;
    bool _ownsActiveRuntime = false;
};

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
    explicit embedded_mongodb_handle(std::string path) : runtime(std::move(path)) {}

    Runtime runtime;
};

extern "C" {

int embedded_mongodb_initialize(embedded_mongodb_log_callback logCallback, char** error) noexcept {
    return translateErrors(error, [logCallback] { runInitializers(logCallback); });
}

int embedded_mongodb_open(const char* path,
                          std::size_t pathLen,
                          embedded_mongodb_handle** handle,
                          char** error) noexcept {
    return translateErrors(error, [&] {
        if (!path || !handle) {
            throw std::invalid_argument("path and handle are required");
        }
        *handle = nullptr;
        *handle = new embedded_mongodb_handle(std::string(path, pathLen));
    });
}

int embedded_mongodb_run_command(embedded_mongodb_handle* handle,
                                  const char* database,
                                  std::size_t databaseLen,
                                  const std::uint8_t* command,
                                  std::size_t commandLen,
                                  embedded_mongodb_buffer* response,
                                  char** error) noexcept {
    return translateErrors(error, [&] {
        if (!handle || !database || !response) {
            throw std::invalid_argument("handle, database, and response are required");
        }

        response->data = nullptr;
        response->len = 0;
        auto bytes =
            handle->runtime.runCommand(std::string_view(database, databaseLen), command, commandLen);
        auto* copy = static_cast<std::uint8_t*>(std::malloc(bytes.size()));
        if (!copy) {
            throw std::bad_alloc();
        }
        std::memcpy(copy, bytes.data(), bytes.size());
        response->data = copy;
        response->len = bytes.size();
    });
}

int embedded_mongodb_close(embedded_mongodb_handle* handle, char** error) noexcept {
    return translateErrors(error, [&] {
        std::unique_ptr<embedded_mongodb_handle> owner(handle);
        if (owner) {
            owner->runtime.close();
        }
    });
}

void embedded_mongodb_free(void* memory) noexcept {
    std::free(memory);
}

}
