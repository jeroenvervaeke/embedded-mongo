#pragma once

#include "embedded_mongodb_native.h"

namespace embedded_mongodb {

/// Runs MongoDB's global initializers, and on the first call installs the log sink that
/// forwards logv2 records to `logCallback`. Everything here is process-wide and happens once,
/// however many times it is called and from wherever: a later call with a different callback
/// changes nothing, because the initializer graph cannot be rerun.
void runInitializers(embedded_mongodb_log_callback logCallback = nullptr);

}  // namespace embedded_mongodb
