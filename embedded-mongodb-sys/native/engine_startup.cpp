#include "engine_startup.h"

#include "engine_identity.h"

#include "mongo/base/initializer.h"
#include "mongo/db/commands.h"
#include "mongo/db/topology/cluster_role.h"
#include "mongo/logv2/attributes.h"
#include "mongo/logv2/component_settings_filter.h"
#include "mongo/logv2/json_formatter.h"
#include "mongo/logv2/log_domain_global.h"
#include "mongo/logv2/log_manager.h"
#include "mongo/logv2/log_severity.h"
#include "mongo/util/assert_util.h"

#include <cstdint>
#include <mutex>
#include <string>
#include <string_view>
#include <vector>

#include <boost/log/attributes/value_extraction.hpp>
#include <boost/log/core/core.hpp>
#include <boost/log/sinks/basic_sink_backend.hpp>
#include <boost/log/sinks/unlocked_frontend.hpp>
#include <boost/smart_ptr/make_shared_object.hpp>

namespace embedded_mongodb {
namespace {

std::once_flag initializersOnce;

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

}  // namespace

void runInitializers(embedded_mongodb_log_callback logCallback) {
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

}  // namespace embedded_mongodb
