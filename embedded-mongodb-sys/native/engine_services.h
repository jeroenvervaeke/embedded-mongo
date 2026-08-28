#pragma once

namespace mongo {
class ServiceContext;
}  // namespace mongo

namespace embedded_mongodb {

/// Installs the process-level services every command path reaches for: the wire
/// specification, authorization, the shard-role service entry point, an op observer, a
/// periodic runner and the direct-client factory. Nothing here touches storage, so it runs
/// before the storage options are decided.
void installProcessServices(mongo::ServiceContext* serviceContext);

/// Installs the services the catalog needs in place before the storage engine starts: the
/// sharding stand-ins, the database holder and collection factory, a replication coordinator
/// that reports a standalone primary, and the index build coordinator.
///
/// Separate from `installProcessServices` because it has to run after `storageGlobalParams`
/// is filled in and before `startUpStorageEngineAndCollectionCatalog`, which reads both.
void installCatalogServices(mongo::ServiceContext* serviceContext);

}  // namespace embedded_mongodb
