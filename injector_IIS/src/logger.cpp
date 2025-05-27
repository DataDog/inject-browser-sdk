// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2024-Present Datadog, Inc.

#include "logger.h"
#include "module_context.h"

namespace datadog::rum {

LogLevel Logger::get_log_level(const ModuleContext *ctx) const {
  if (ctx != nullptr) {
    return ctx->log_level;
  }
  return LogLevel::Info;
}

} // namespace datadog::rum
