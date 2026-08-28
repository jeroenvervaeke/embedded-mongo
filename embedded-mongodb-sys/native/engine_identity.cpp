#include "engine_identity.h"

#include "mongo/db/commands.h"
#include "mongo/db/commands/server_status/server_status.h"
#include "mongo/db/service_context.h"
#include "mongo/util/version.h"

#include <string>
#include <string_view>
#include <vector>

namespace embedded_mongodb {
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

}  // namespace

void installEmbeddedVersionInfo() {
    // kFallback rather than the default: aborting the host process over an Easter egg would
    // be a poor trade. If nothing installed real version information, the fallback is what
    // gets decorated.
    static const EmbeddedVersionInfo embedded{mongo::VersionInfoInterface::instance(
        mongo::VersionInfoInterface::NotEnabledAction::kFallback)};
    mongo::VersionInfoInterface::enable(&embedded);
}

}  // namespace embedded_mongodb
