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
#include <utility>
#include <unordered_map>

namespace datadog::rum {

// Owns a StableConfig handle returned by stable_config_read*.
struct StableConfigDeleter {
  void operator()(::StableConfig* config) const { stable_config_cleanup(config); }
};

// Language selector for stable configuration: matched against `origin:
// language` rules in application_monitoring.yaml.
inline constexpr const char* k_stable_config_language = "browser-sdk";

// Test seam. GlobalModule reads application_monitoring.yaml from machine-global
// paths, which makes any test that asserts "nothing is configured" depend on
// whether the host happens to have an agent-written file. Point the read
// somewhere that doesn't exist instead. NULL restores the platform default.
//
// Not thread-safe and not for production use: call before RegisterModule.
void set_stable_config_paths_for_testing(const char* local_path, const char* fleet_path);

// The paths GlobalModule should read; NULL members mean "platform default".
std::pair<const char*, const char*> stable_config_paths();

// The `majorVersion` used on the IIS-only path, where snippet_create_from_json
// requires the field and there is no stable configuration to supply it. Matches
// both RUM_schema.xml's defaultValue and the library's own
// DEFAULT_MAJOR_VERSION, so a site gets the same major version however it was
// configured.
inline constexpr int k_iis_default_major_version = 6;

struct RumConfig {  // NOLINT(bugprone-exception-escape)
  std::optional<int> version;
  std::unordered_map<std::string, std::string> sdk_options;
  // Where the snippet is injected. Configures the module rather than the Browser SDK, so it is
  // read as its own attribute instead of going through the `option` collection.
  InjectionStrategy injection_strategy = InjectionStrategy::HeadCloseTag;

  // "Nobody configured anything here." Deliberately ignores `version`: once the
  // MSI registers datadogRum in configSections, IIS materialises the section
  // from RUM_schema.xml for every site, so `version` carries the schema default
  // even where no <datadogRum> element exists. The SDK options collection is
  // the only part that is genuinely user-supplied.
  bool empty() const { return sdk_options.empty(); }
};

enum class ReadConfStatus {
  Ok,        // Config read successfully.
  NotFound,  // No IIS config here — stable configuration may still apply.
  Disabled,  // Explicitly switched off — clear existing snippet, ignore stable
             // configuration.
  Error,     // Transient error — keep existing snippet.
};

struct ReadConfResult {
  static ReadConfResult ok(RumConfig config) { return {ReadConfStatus::Ok, std::move(config)}; }
  static ReadConfResult not_found(std::optional<RumConfig> config = std::nullopt) {
    return {ReadConfStatus::NotFound, std::move(config)};
  }
  static ReadConfResult disabled() { return {ReadConfStatus::Disabled}; }
  static ReadConfResult error() { return {ReadConfStatus::Error}; }

  ReadConfStatus status;
  std::optional<RumConfig> config =
      std::nullopt;  // Also retains module settings for an empty IIS section.
};

struct ServerConfig {
  LogLevel log_level;
};

std::string wstring_to_utf8(const std::wstring& wstr);

ReadConfResult read_conf(const IHttpServer& server, PCWSTR cfg_path, Logger& logger);

std::optional<ServerConfig> read_server_conf(const IHttpServer& server, Logger& logger);

// Renders IIS-sourced settings as the JSON document the Rust library consumes.
//
// Shared by both snippet paths so the per-key typing (numeric sample rates,
// boolean track* flags, comma-separated allowedTracingUrls) can't drift between
// them. `major_version` and `enabled` are emitted only when engaged, which is
// what makes the result usable as an *overlay*: an omitted key leaves the
// stable-config value alone.
//
// The top-level shape must stay within {majorVersion, enabled, rum} —
// ConfigurationOverlay is deny_unknown_fields, so one stray key rejects the
// whole overlay.
//
// Exported so tests can exercise the typing without standing up COM mocks.
std::string make_config_json(std::optional<int> major_version, std::optional<bool> enabled,
                             const std::unordered_map<std::string, std::string>& opts,
                             Logger& logger);

// Resolves the effective configuration for `config_path` and swaps the result
// into `ctx`.
//
// `stable_config` is the process-wide handle read once at startup, borrowed and
// not owned; it may be null, in which case only the IIS section is consulted.
// When it is non-null, the IIS section acts as a per-key overlay on top of it.
//
// Error preserves ctx.snippet/tags so a transient COM read failure can
// self-heal on the next OnGlobalConfigurationChange.
void update_module_context(ModuleContext& ctx, const IHttpServer& server, PCWSTR config_path,
                           Logger& logger, const ::StableConfig* stable_config);

}  // namespace datadog::rum
