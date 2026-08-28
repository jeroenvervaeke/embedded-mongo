#include "engine_sharding.h"

#include "mongo/db/shard_role/shard_catalog/collection_metadata.h"
#include "mongo/db/shard_role/shard_catalog/scoped_collection_metadata.h"

#include <memory>

namespace embedded_mongodb {
namespace {

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

}  // namespace

std::unique_ptr<mongo::CollectionShardingStateFactory> makeCollectionShardingStateFactory() {
    return std::make_unique<StandaloneCollectionShardingStateFactory>();
}

std::unique_ptr<mongo::DatabaseShardingStateFactory> makeDatabaseShardingStateFactory() {
    return std::make_unique<StandaloneDatabaseShardingStateFactory>();
}

}  // namespace embedded_mongodb
