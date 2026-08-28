#pragma once

#include "mongo/db/shard_role/shard_catalog/collection_sharding_state.h"
#include "mongo/db/shard_role/shard_catalog/database_sharding_state.h"

#include <memory>

namespace embedded_mongodb {

/// Factories that answer every sharding question with "this node owns all of it", installed
/// on the ServiceContext in place of the ones the sharding runtime would provide. See
/// engine_sharding.cpp for why the real ones are not linked.
std::unique_ptr<mongo::CollectionShardingStateFactory> makeCollectionShardingStateFactory();
std::unique_ptr<mongo::DatabaseShardingStateFactory> makeDatabaseShardingStateFactory();

}  // namespace embedded_mongodb
