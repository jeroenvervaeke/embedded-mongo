#include "embedded_mongodb_native.h"

#include "mongo/base/initializer.h"
#include "mongo/bson/bson_validate.h"
#include "mongo/db/auth/authorization_manager.h"
#include "mongo/db/auth/authorization_manager_factory_impl.h"
#include "mongo/db/client_strand.h"
#include "mongo/db/commands.h"
#include "mongo/db/commands/server_status/server_status.h"
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
#include "mongo/db/shard_role/shard_catalog/catalog_repair.h"
#include "mongo/db/shard_role/shard_catalog/collection.h"
#include "mongo/db/shard_role/shard_catalog/collection_catalog.h"
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
#include "mongo/util/version.h"
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

// Who built this engine, and how to tell it apart from a real mongod.
//
// The engine is reached in-process, so nothing can connect a shell or Compass to it to find
// out what it is. These three surfaces are how a caller -- or a test -- answers "am I talking
// to the embedded engine?": `buildInfo` reports an "embedded" module, `serverStatus` grows an
// `embedded` section, and `embeddedMongodb` is a command of its own. All three are registered
// from this file rather than from a patch, so a submodule bump cannot quietly drop them.
constexpr std::string_view kEmbeddedAuthor = "Jeroen Vervaeke";
constexpr std::string_view kEmbeddedRepository = "https://github.com/jeroenvervaeke/embedded-mongo";
constexpr std::string_view kEmbeddedTrue = "true";

void appendEmbeddedInfo(mongo::BSONObjBuilder* builder) {
    builder->append("embedded", true);
    builder->append("author", std::string{kEmbeddedAuthor});
    builder->append("repository", std::string{kEmbeddedRepository});
    // The engine's own version, so a caller can tell which MongoDB is inside without
    // this file having to restate something that moves with the submodule.
    builder->append(
        "mongoVersion",
        std::string{mongo::VersionInfoInterface::instance(
                        mongo::VersionInfoInterface::NotEnabledAction::kFallback)
                        .version()});
}

/// Decorates the real version information with an "embedded" module and two extra buildInfo
/// fields. Everything else is delegated, so `explain` and anything else that reports server
/// version keeps working exactly as before -- the reason //src/mongo/util:version_impl is a
/// dependency in the first place.
class EmbeddedVersionInfo final : public mongo::VersionInfoInterface {
public:
    explicit EmbeddedVersionInfo(const VersionInfoInterface& base) : _base(base) {}

    int majorVersion() const override {
        return _base.majorVersion();
    }
    int minorVersion() const override {
        return _base.minorVersion();
    }
    int patchVersion() const override {
        return _base.patchVersion();
    }
    int extraVersion() const override {
        return _base.extraVersion();
    }
    std::string_view version() const override {
        return _base.version();
    }
    std::string_view gitVersion() const override {
        // Deliberately untouched. This reports which MongoDB the engine was built from, and
        // overwriting it with anything of ours would misattribute the engine's provenance.
        return _base.gitVersion();
    }
    std::string_view allocator() const override {
        return _base.allocator();
    }
    std::string_view jsEngine() const override {
        return _base.jsEngine();
    }
    std::string_view targetMinOS() const override {
        return _base.targetMinOS();
    }

    std::vector<std::string_view> modules() const override {
        auto modules = _base.modules();
        modules.emplace_back("embedded");
        return modules;
    }

    std::vector<BuildInfoField> buildInfo() const override {
        auto fields = _base.buildInfo();
        // The views must outlive the call; all three point at static storage.
        fields.push_back({"embedded", kEmbeddedTrue, true, true});
        fields.push_back({"embeddedAuthor", kEmbeddedAuthor, true, true});
        fields.push_back({"embeddedRepository", kEmbeddedRepository, true, false});
        return fields;
    }

private:
    const VersionInfoInterface& _base;
};

void installEmbeddedVersionInfo() {
    // kFallback rather than the default: aborting the host process over an Easter egg would
    // be a poor trade. If nothing installed real version information, the fallback is what
    // gets decorated.
    static const EmbeddedVersionInfo embedded{mongo::VersionInfoInterface::instance(
        mongo::VersionInfoInterface::NotEnabledAction::kFallback)};
    mongo::VersionInfoInterface::enable(&embedded);
}

/// `db.runCommand({embeddedMongodb: 1})`.
class EmbeddedMongodbCommand final : public mongo::BasicCommand {
public:
    EmbeddedMongodbCommand() : BasicCommand("embeddedMongodb") {}

    AllowedOnSecondary secondaryAllowed(mongo::ServiceContext*) const override {
        return AllowedOnSecondary::kAlways;
    }

    bool supportsWriteConcern(const mongo::BSONObj&) const override {
        return false;
    }

    std::string help() const override {
        return "reports that this is the embedded MongoDB engine, and who built it";
    }

    mongo::Status checkAuthForOperation(mongo::OperationContext*,
                                        const mongo::DatabaseName&,
                                        const mongo::BSONObj&) const override {
        return mongo::Status::OK();
    }

    bool requiresAuthzChecks() const override {
        return false;
    }

    bool run(mongo::OperationContext*,
             const mongo::DatabaseName&,
             const mongo::BSONObj&,
             mongo::BSONObjBuilder& result) override {
        appendEmbeddedInfo(&result);
        return true;
    }
};
MONGO_REGISTER_COMMAND(EmbeddedMongodbCommand).forShard();

/// The `embedded` section of `db.serverStatus()`.
class EmbeddedServerStatusSection final : public mongo::ServerStatusSection {
public:
    using mongo::ServerStatusSection::ServerStatusSection;

    bool includeByDefault() const override {
        return true;
    }

    mongo::BSONObj generateSection(mongo::OperationContext*,
                                   const mongo::BSONElement&) const override {
        mongo::BSONObjBuilder builder;
        appendEmbeddedInfo(&builder);
        return builder.obj();
    }
};
auto& gEmbeddedServerStatusSection =
    *mongo::ServerStatusSectionBuilder<EmbeddedServerStatusSection>("embedded").forShard();

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
        // After runGlobalInitializers, so this wraps the real implementation rather than
        // racing the static initializer in //src/mongo/util:version_impl that installs it.
        installEmbeddedVersionInfo();
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

        // The shutdown state is what tells the reconciliation below whether the internal idents
        // left in the directory are the remains of a resumable index build or rubbish from a
        // process that was killed. mongod threads the same value into its startup recovery.
        const auto lastShutdownState = mongo::catalog::startUpStorageEngineAndCollectionCatalog(
            _serviceContext, clientGuard.get(), mongo::StorageEngineInitFlags{});
        _storageStarted = true;
        mongo::StorageControl::startStorageControls(_serviceContext);

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
        {
            auto opCtx = _serviceContext->makeOperationContext(clientGuard.get());
            mongo::Lock::GlobalWrite globalLock(opCtx.get());

            auto* storageEngine = _serviceContext->getStorageEngine();
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
