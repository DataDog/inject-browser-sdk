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

// The values web.config sets explicitly. Each one overrides stable configuration
// (application_monitoring.yaml); unset ones fall back to it, then to the defaults.
struct RumConfig {  // NOLINT(bugprone-exception-escape)
  // Set only when `enabled="true"` is explicit: it then overrides a stable-config opt-out.
  std::optional<bool> enabled;
  // The Browser SDK major version, the `version` attribute.
  std::optional<int> version;
  // The `option` collection: Browser SDK init options by name, in their text form.
  std::unordered_map<std::string, std::string> sdk_options;
  // Where the snippet is injected, the `injectionStrategy` attribute.
  std::optional<InjectionStrategy> injection_strategy;
  // Whether static files are read to be injected, the `injectStaticFiles` attribute.
  std::optional<bool> inject_static_files;
};

enum class ReadConfStatus {
  Ok,      // Config read successfully.
  Absent,  // Config missing or disabled — clear existing snippet.
  Error,   // Transient error — keep existing snippet.
};

struct ReadConfResult {
  static ReadConfResult ok(RumConfig config) { return {ReadConfStatus::Ok, std::move(config)}; }
  static ReadConfResult absent() { return {ReadConfStatus::Absent}; }
  static ReadConfResult error() { return {ReadConfStatus::Error}; }

  ReadConfStatus status;
  std::optional<RumConfig> config = std::nullopt;  // Set only when status == Ok.
};

struct ServerConfig {
  LogLevel log_level;
};

std::string wstring_to_utf8(const std::wstring& wstr);

ReadConfResult read_conf(const IHttpServer& server, PCWSTR cfg_path, Logger& logger);

std::optional<ServerConfig> read_server_conf(const IHttpServer& server, Logger& logger);

// Error preserves ctx.snippet/tags so a transient COM read failure can
// self-heal on the next OnGlobalConfigurationChange.
void update_module_context(ModuleContext& ctx, const IHttpServer& server, PCWSTR config_path,
                           Logger& logger);

}  // namespace datadog::rum
