/*
 * Unless explicitly stated otherwise all files in this repository are licensed
 * under the Apache 2.0 License. This product includes software developed at
 * Datadog (https://www.datadoghq.com/).
 *
 * Copyright 2024-Present Datadog, Inc.
 */

#pragma once

#include "framework.h"
#include "logger.h"
#include "module_context.h"

#include <optional>
#include <string>
#include <unordered_map>

namespace datadog::rum {

struct RumConfig {
  int version;
  std::unordered_map<std::string, std::string> sdk_options;
};

enum class ReadConfStatus {
  Ok,          // Config read successfully.
  Absent,      // Config missing or disabled — clear existing snippet.
  Error,       // Transient error — keep existing snippet.
};

struct ReadConfResult {
  ReadConfStatus status;
  std::optional<RumConfig> config;  // Set only when status == Ok.
};

struct ServerConfig {
  LogLevel log_level;
};

std::string wstring_to_utf8(const std::wstring &wstr);

ReadConfResult read_conf(const IHttpServer &server, PCWSTR cfg_path,
                         Logger &logger);

std::optional<ServerConfig> read_server_conf(const IHttpServer &server,
                                             Logger &logger);

// Reads config for the given path and updates ctx.snippet/tags. When config
// is missing, invalid, or disabled, clears the existing snippet to disable
// injection.
void update_module_context(ModuleContext &ctx, const IHttpServer &server,
                           PCWSTR config_path, Logger &logger);

} // namespace datadog::rum
