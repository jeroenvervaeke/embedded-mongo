#include "engine_services.h"

#include "engine_sharding.h"

#include "mongo/db/auth/authorization_manager.h"
#include "mongo/db/auth/authorization_manager_factory_impl.h"
#include "mongo/db/dbdirectclient.h"
#include "mongo/db/index_builds/index_builds_coordinator.h"
#include "mongo/db/index_builds/index_builds_coordinator_mongod.h"
#include "mongo/db/op_observer/op_observer_registry.h"
#include "mongo/db/repl/repl_settings.h"
#include "mongo/db/repl/replication_coordinator.h"
#include "mongo/db/repl/replication_coordinator_mock.h"
#include "mongo/db/service_context.h"
#include "mongo/db/service_entry_point_shard_role.h"
#include "mongo/db/shard_role/shard_catalog/collection.h"
#include "mongo/db/shard_role/shard_catalog/collection_impl.h"
#include "mongo/db/shard_role/shard_catalog/database_holder.h"
#include "mongo/db/shard_role/shard_catalog/database_holder_impl.h"
#include "mongo/db/topology/sharding_state.h"
#include "mongo/db/wire_version.h"
#include "mongo/scripting/dbdirectclient_factory.h"
#include "mongo/util/assert_util.h"
#include "mongo/util/periodic_runner_factory.h"

#include <memory>
#include <utility>

namespace embedded_mongodb {

void installProcessServices(mongo::ServiceContext* serviceContext) {
    mongo::WireSpec::Specification wireSpec;
    wireSpec.isInternalClient = true;
    mongo::WireSpec::getWireSpec(serviceContext).initialize(std::move(wireSpec));

    auto authFactory = std::make_unique<mongo::AuthorizationManagerFactoryImpl>();
    mongo::AuthorizationManager::set(serviceContext->getService(),
                                     authFactory->createShard(serviceContext->getService()));
    // There is no network and no second principal: the only client is the process that linked
    // this library, and it already has the file permissions on the data directory.
    mongo::AuthorizationManager::get(serviceContext->getService())->setAuthEnabled(false);

    serviceContext->getService()->setServiceEntryPoint(
        std::make_unique<mongo::ServiceEntryPointShardRole>());
    serviceContext->setOpObserver(std::make_unique<mongo::OpObserverRegistry>());
    serviceContext->setPeriodicRunner(mongo::makePeriodicRunner(serviceContext));

    mongo::DBDirectClientFactory::get(serviceContext)
        .registerImplementation([](mongo::OperationContext* opCtx) {
            return std::make_unique<mongo::DBDirectClient>(opCtx);
        });
}

void installCatalogServices(mongo::ServiceContext* serviceContext) {
    mongo::ShardingState::create(serviceContext);
    mongo::CollectionShardingStateFactory::set(serviceContext,
                                               makeCollectionShardingStateFactory());
    mongo::DatabaseShardingStateFactory::set(serviceContext, makeDatabaseShardingStateFactory());
    mongo::DatabaseHolder::set(serviceContext, std::make_unique<mongo::DatabaseHolderImpl>());
    mongo::Collection::Factory::set(serviceContext,
                                    std::make_unique<mongo::CollectionImplFactory>());

    // A mock rather than the real coordinator, held permanently at primary: nothing here
    // replicates, but the shard role asks on every write whether this node may accept one.
    auto replCoordinator = std::make_unique<mongo::repl::ReplicationCoordinatorMock>(
        serviceContext, mongo::repl::ReplSettings());
    uassertStatusOK(replCoordinator->setFollowerMode(mongo::repl::MemberState::RS_PRIMARY));
    mongo::repl::ReplicationCoordinator::set(serviceContext, std::move(replCoordinator));

    mongo::IndexBuildsCoordinator::set(serviceContext,
                                       std::make_unique<mongo::IndexBuildsCoordinatorMongod>());
}

}  // namespace embedded_mongodb
