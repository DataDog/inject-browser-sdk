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

// Owns a StableConfig handle returned by stable_config_read*.
struct StableConfigDeleter {
  void operator()(::StableConfig* config) const { stable_config_cleanup(config); }
};

// Matches `origin: language` rules in application_monitoring.yaml.
inline constexpr const char* k_stable_config_language = "browser-sdk";

using StableConfigReader = ::StableConfig* (*)(const char*, bool);
// Call before RegisterModule; nullptr restores the production reader.
void set_stable_config_reader_for_testing(StableConfigReader reader);
::StableConfig* read_stable_config();

// IIS-only default; matches RUM_schema.xml and the Rust library's default.
inline constexpr int k_iis_default_major_version = 6;

struct RumConfig {  // NOLINT(bugprone-exception-escape)
  std::optional<int> version;
  std::unordered_map<std::string, std::string> sdk_options;
  // Where the snippet is injected. Configures the module rather than the Browser SDK, so it is
  // read as its own attribute instead of going through the `option` collection.
  InjectionStrategy injection_strategy = InjectionStrategy::HeadCloseTag;
};

enum class ReadConfStatus {
  Ok,        // Config read successfully.
  NotFound,  // No IIS config here — stable configuration may still apply.
  Disabled,  // Explicitly switched off — clear existing snippet, ignore stable
             // configuration.
  Error,     // Transient error — keep existing snippet.
};

struct ReadConfResult {
  ReadConfStatus status;
  RumConfig config{};  // Retains module settings even for an empty IIS section.
};

struct ServerConfig {
  LogLevel log_level;
};

std::string wstring_to_utf8(const std::wstring& wstr);

ReadConfResult read_conf(const IHttpServer& server, PCWSTR cfg_path, Logger& logger);

std::optional<ServerConfig> read_server_conf(const IHttpServer& server, Logger& logger);

// Renders typed IIS options for both JSON and stable-config snippets. Omitting
// major_version lets stable configuration supply it. IIS opt-outs are handled
// separately, so this never overrides stable configuration's enabled switch.
std::string make_config_json(std::optional<int> major_version,
                             const std::unordered_map<std::string, std::string>& opts,
                             Logger& logger);

// Applies an IIS overlay to the borrowed process-wide stable_config (or uses
// IIS alone if null). Errors preserve the previous snippet and tags for reload.
void update_module_context(ModuleContext& ctx, const IHttpServer& server, PCWSTR config_path,
                           Logger& logger, const ::StableConfig* stable_config);

}  // namespace datadog::rum
