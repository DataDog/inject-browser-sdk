// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2025-Present Datadog, Inc.

#include "config_reader.h"
#include "defer.h"
#include "injectbrowsersdk.h"
#include "module_context.h"
#include "telemetry.h"

#include <cassert>
#include <datadog/telemetry/telemetry.h>
#include <format>
#include <string>
#include <string_view>

namespace datadog::rum {
namespace {

// Stable-configuration keys for the module settings outside the Browser SDK configuration.
constexpr const char* kStableInjectionStrategy = "DD_RUM_INJECTION_STRATEGY";
constexpr const char* kStableInjectStaticFiles = "DD_RUM_INJECT_STATIC_FILES";
constexpr const char* kStableLogLevel = "DD_RUM_IIS_LOG_LEVEL";

// Set only by tests, before the first read. Empty uses the standard path.
std::string& test_stable_config_path(bool fleet) {
  static std::string local_path;
  static std::string fleet_path;
  return fleet ? fleet_path : local_path;
}

const char* path_or_default(const std::string& path) {
  return path.empty() ? nullptr : path.c_str();
}

// Stable configuration is read once per worker: every application sees one snapshot, and a
// change to the YAML takes a recycle.
StableConfig* stable_config_for_worker() {
  static StableConfig* config = stable_config_read_with_paths(
      "browser-sdk", false, path_or_default(test_stable_config_path(false)),
      path_or_default(test_stable_config_path(true)));
  return config;
}

// Maps a stable-configuration value to one of `values`, by case-insensitive name. Returns
// `fallback` when the key is unset or the value unknown.
template <typename T, size_t N>
T stable_choice(const char* key, const std::pair<const char*, T> (&values)[N], T fallback,
                Logger& logger) {
  const char* raw = stable_config_get(stable_config_for_worker(), key);
  if (raw == nullptr) return fallback;
  for (const auto& [name, value] : values) {
    if (_stricmp(raw, name) == 0) return value;
  }
  logger.error(std::format("Invalid stable configuration value '{}' for {}", raw, key));
  return fallback;
}

InjectionStrategy stable_injection_strategy(Logger& logger) {
  constexpr std::pair<const char*, InjectionStrategy> values[] = {
      {"head_close_tag", InjectionStrategy::HeadCloseTag},
      {"head_open_tag", InjectionStrategy::HeadOpenTag}};
  return stable_choice(kStableInjectionStrategy, values, InjectionStrategy::HeadCloseTag, logger);
}

bool stable_inject_static_files(Logger& logger) {
  constexpr std::pair<const char*, bool> values[] = {{"true", true}, {"false", false}};
  return stable_choice(kStableInjectStaticFiles, values, true, logger);
}

LogLevel stable_log_level(Logger& logger) {
  constexpr std::pair<const char*, LogLevel> values[] = {
      {"error", LogLevel::Error}, {"info", LogLevel::Info}, {"debug", LogLevel::Debug}};
  return stable_choice(kStableLogLevel, values, LogLevel::Info, logger);
}

void clear_module_snippet(ModuleContext& ctx) {
  if (ctx.snippet != nullptr) {
    snippet_cleanup(ctx.snippet);
    ctx.snippet = nullptr;
  }
  ctx.application_id_tag.clear();
  ctx.remote_config_tag.clear();
}

IAppHostProperty* get_property(IAppHostElement& xmlElement, std::wstring_view name) {
  IAppHostProperty* property = nullptr;
  if (xmlElement.GetPropertyByName((BSTR)name.data(), &property) != S_OK) {
    return nullptr;
  }

  return property;
}

// Returns a VT_EMPTY variant if the COM call fails or the property has no
// value. Callers must check v.vt before accessing union members, and must
// call VariantClear on the returned VARIANT when done.
VARIANT get_property_value(IAppHostProperty& property) {
  VARIANT v;
  VariantInit(&v);
  HRESULT const hr = property.get_Value(&v);
  if (FAILED(hr)) {
    VariantClear(&v);
  }
  return v;
}

// A scalar attribute: `bool`, `int` and `enum` attributes all read as an integer.
struct Attribute {
  std::optional<int> value;  // Unset when the attribute is missing or of another type.
  // Whether the value is the schema default, set by no configuration file. Metadata that
  // cannot be read counts as explicit, so the IIS value keeps precedence.
  bool is_default = false;

  // The value, only when a configuration file sets it.
  std::optional<int> explicit_value() const { return is_default ? std::nullopt : value; }
};

Attribute read_attribute(IAppHostElement& element, std::wstring_view name, Logger& logger) {
  auto* property = get_property(element, name);
  if (property == nullptr) return {};
  const auto defer_property = defer([&] { property->Release(); });

  Attribute attribute;
  VARIANT is_default;
  VariantInit(&is_default);
  if (SUCCEEDED(property->GetMetadata((BSTR)L"isDefaultValue", &is_default))) {
    attribute.is_default = is_default.vt == VT_BOOL && is_default.boolVal == VARIANT_TRUE;
  }
  VariantClear(&is_default);

  auto value = get_property_value(*property);
  const auto defer_value = defer([&] { VariantClear(&value); });
  switch (value.vt) {
    case VT_I4:
      attribute.value = value.lVal;
      break;
    case VT_I2:
      attribute.value = value.iVal;
      break;
    case VT_BOOL:
      attribute.value = value.boolVal != VARIANT_FALSE;
      break;
    case VT_EMPTY:
      break;
    default:
      logger.info(std::format("Unexpected '{}' type (vt={}), using the default",
                              wstring_to_utf8(std::wstring(name)), value.vt));
  }
  return attribute;
}

// The string value of `name` on an option element, or nullopt when missing or not a string.
std::optional<std::string> read_string(IAppHostElement& element, std::wstring_view name) {
  auto* property = get_property(element, name);
  if (property == nullptr) return std::nullopt;
  const auto defer_property = defer([&] { property->Release(); });
  auto value = get_property_value(*property);
  const auto defer_value = defer([&] { VariantClear(&value); });
  if (value.vt != VT_BSTR || value.bstrVal == nullptr) return std::nullopt;
  return wstring_to_utf8(std::wstring(value.bstrVal));
}

// Reads the effective `option` collection, inheritance already applied by IIS. A collection
// read failure returns false so the caller keeps its last valid configuration; malformed
// entries are skipped.
bool read_sdk_options(IAppHostElement& element,
                      std::unordered_map<std::string, std::string>& rum_sdk_opts, Logger& logger) {
  IAppHostElementCollection* col = nullptr;
  HRESULT const hr_col = element.get_Collection(&col);
  if (FAILED(hr_col)) {
    logger.error(std::format("Failed to read SDK options collection: {}",
                             std::system_category().message(hr_col)));
    return false;
  }
  if (col == nullptr) return true;
  const auto defer_col = defer([&] { col->Release(); });

  DWORD n_elem = 0;
  HRESULT const hr_count = col->get_Count(&n_elem);
  if (FAILED(hr_count)) {
    logger.error(std::format("Failed to read SDK option count: {}",
                             std::system_category().message(hr_count)));
    return false;
  }

  VARIANT item_idx;
  item_idx.vt = VT_UI4;
  for (DWORD i = 0; i < n_elem; ++i) {
    item_idx.ulVal = i;
    IAppHostElement* option_element = nullptr;
    if (FAILED(col->get_Item(item_idx, &option_element)) || option_element == nullptr) {
      logger.debug(std::format("Skipping config option at index {}: failed to read element", i));
      continue;
    }
    const auto option_element_guard = defer([&] { option_element->Release(); });

    auto name = read_string(*option_element, L"name");
    auto value = read_string(*option_element, L"value");
    if (!name || !value) {
      logger.debug(std::format("Skipping config option at index {}: missing name or value", i));
      continue;
    }
    rum_sdk_opts.emplace(std::move(*name), std::move(*value));
  }
  return true;
}

// The explicit web.config values as snippet settings, or nullptr when there are none: the
// snippet then comes from stable configuration alone. Values that do not fit their option are
// skipped, the rest still apply.
SnippetConfig* make_snippet_config(const RumConfig& config, Logger& logger) {
  if (!config.enabled && !config.version && config.sdk_options.empty()) return nullptr;
  SnippetConfig* settings = snippet_config_create();
  for (const auto& [name, value] : config.sdk_options) {
    // These names belong to the `enabled` and `version` attributes.
    if (name == "enabled" || name == "majorVersion") continue;
    if (!snippet_config_set_text(settings, name.c_str(), value.c_str())) {
      logger.error(std::format("Invalid value '{}' for config key '{}', skipping it", value, name));
    }
  }
  if (config.enabled) snippet_config_set_bool(settings, "enabled", *config.enabled);
  if (config.version) snippet_config_set_number(settings, "majorVersion", *config.version);
  return settings;
}

Snippet* make_snippet(const RumConfig& config, Logger& logger) {
  SnippetConfig* settings = make_snippet_config(config, logger);
  const auto defer_settings = defer([&] { snippet_config_cleanup(settings); });
  Snippet* snippet = stable_config_snippet_create_with_config(stable_config_for_worker(), settings);
  if (snippet == nullptr || snippet->error_code != 6 || !config.enabled) return snippet;

  // Unreadable stable configuration cannot be trusted not to hold an opt-out, so only an
  // explicit `enabled="true"` builds from web.config alone.
  Snippet* manual = stable_config_snippet_create_with_config(nullptr, settings);
  if (manual == nullptr || manual->error_code != 0) {
    snippet_cleanup(manual);
    return snippet;
  }
  snippet_cleanup(snippet);
  return manual;
}

}  // namespace

void set_stable_config_paths_for_testing(std::string local_path, std::string fleet_path) {
  test_stable_config_path(false) = std::move(local_path);
  test_stable_config_path(true) = std::move(fleet_path);
}

std::string wstring_to_utf8(const std::wstring& wstr) {
  if (wstr.empty()) return {};
  const int size_needed =
      WideCharToMultiByte(CP_UTF8, 0, wstr.data(), (int)wstr.size(), nullptr, 0, nullptr, nullptr);
  if (size_needed <= 0) return {};
  std::string result(size_needed, 0);
  int const written = WideCharToMultiByte(CP_UTF8, 0, wstr.data(), (int)wstr.size(), result.data(),
                                          size_needed, nullptr, nullptr);
  if (written <= 0) return {};
  return result;
}

std::optional<ServerConfig> read_server_conf(const IHttpServer& server, Logger& logger) {
  const ServerConfig default_config{stable_log_level(logger)};
  auto* admin_manager = server.GetAdminManager();
  if (admin_manager == nullptr) {
    logger.debug("Admin manager is null, using default server config");
    return default_config;
  }

  IAppHostElement* cfg_root_elem = nullptr;
  const auto res = admin_manager->GetAdminSection((BSTR)L"system.applicationHost/datadogRumServer",
                                                  (BSTR)L"MACHINE/WEBROOT/APPHOST", &cfg_root_elem);
  if (res != S_OK) {
    // Section may legitimately not exist if the user hasn't configured it.
    logger.debug("datadogRumServer section not found, using default config");
    return default_config;
  }
  const auto defer_root = defer([&] { cfg_root_elem->Release(); });

  const auto level = read_attribute(*cfg_root_elem, L"logLevel", logger).explicit_value();
  if (level && *level >= static_cast<int>(LogLevel::Error) &&
      *level <= static_cast<int>(LogLevel::Debug)) {
    return ServerConfig{static_cast<LogLevel>(*level)};
  }
  return default_config;
}

ReadConfResult read_conf(const IHttpServer& server, PCWSTR cfg_path, Logger& logger) {
  auto* admin_manager = server.GetAdminManager();
  if (admin_manager == nullptr) {
    logger.debug("Admin manager is null, cannot read RUM config");
    return ReadConfResult::error();
  }

  IAppHostElement* cfg_root_elem = nullptr;
  const auto res = admin_manager->GetAdminSection((BSTR)L"system.webServer/datadogRum",
                                                  const_cast<BSTR>(cfg_path), &cfg_root_elem);
  if (res != S_OK) {
    logger.debug(std::format("datadogRum config section not found: {}",
                             std::system_category().message(res)));
    return ReadConfResult::absent();
  }
  const auto defer_root = defer([&] { cfg_root_elem->Release(); });

  const auto enabled = read_attribute(*cfg_root_elem, L"enabled", logger);
  if (!enabled.value) {
    // A missing attribute and a COM failure look alike; treat both as Error so a transient
    // failure can't silently wipe injection.
    logger.debug("Failed to read 'enabled' property");
    return ReadConfResult::error();
  }
  if (*enabled.value == 0) {
    logger.info("RUM injection disabled for this path");
    return ReadConfResult::absent();
  }

  RumConfig config;
  if (enabled.explicit_value()) config.enabled = true;
  config.version = read_attribute(*cfg_root_elem, L"version", logger).explicit_value();
  if (auto strategy =
          read_attribute(*cfg_root_elem, L"injectionStrategy", logger).explicit_value()) {
    if (*strategy >= 0 && *strategy <= static_cast<int>(InjectionStrategy::HeadOpenTag)) {
      config.injection_strategy = static_cast<InjectionStrategy>(*strategy);
    } else {
      logger.info(
          std::format("Out of range 'injectionStrategy' value ({}), ignoring it", *strategy));
    }
  }
  if (auto inject = read_attribute(*cfg_root_elem, L"injectStaticFiles", logger).explicit_value()) {
    config.inject_static_files = *inject != 0;
  }
  if (!read_sdk_options(*cfg_root_elem, config.sdk_options, logger)) {
    return ReadConfResult::error();
  }
  return ReadConfResult::ok(std::move(config));
}

void update_module_context(ModuleContext& ctx, const IHttpServer& server, PCWSTR config_path,
                           Logger& logger) {
  auto result = read_conf(server, config_path, logger);
  if (result.status == ReadConfStatus::Error) {
    logger.info("Keeping existing config after transient read error");
    return;
  }
  if (result.status == ReadConfStatus::Absent) {
    clear_module_snippet(ctx);
    ctx.injection_strategy = InjectionStrategy::HeadCloseTag;
    ctx.inject_static_files = true;
    return;
  }

  if (!result.config) {
    // ReadConfResult::ok is the only way to get here, and it always carries a config.
    logger.error("Config read reported success without a config; keeping previous config");
    return;
  }
  const auto& config = *result.config;
  Snippet* snippet = make_snippet(config, logger);
  if (snippet == nullptr) {
    logger.error("Failed to create RUM snippet; keeping previous config");
    return;
  }
  if (snippet->error_code == 10) {
    // Neither web.config nor stable configuration configures RUM here.
    logger.debug("No RUM configuration for this path");
    snippet_cleanup(snippet);
    clear_module_snippet(ctx);
    return;
  }
  if (snippet->error_code == 0 && snippet->rum_enabled == RUM_ENABLED_FALSE) {
    logger.info("RUM injection disabled by stable configuration");
    snippet_cleanup(snippet);
    clear_module_snippet(ctx);
    return;
  }
  if (snippet->error_code != 0) {
    // A broken stable configuration may hold an opt-out, so it clears the previous snippet.
    // Any other failure keeps it: continued injection with the last-known-good config beats
    // going dark on a broken deploy, and the next valid reload replaces it.
    const bool clear = snippet->error_code == 6;
    logger.error(std::format("Failed to create RUM snippet: {}; {} previous config",
                             snippet->error_message, clear ? "clearing" : "keeping"));
    datadog::telemetry::counter::increment(
        telemetry::config_invalid,
        telemetry::build_tags(std::format("error_code:{}", snippet->error_code),
                              ctx.application_id_tag, ctx.remote_config_tag));
    snippet_cleanup(snippet);
    if (clear) clear_module_snippet(ctx);
    return;
  }

  logger.info("Configuration validated");
  if (ctx.snippet != nullptr) {
    snippet_cleanup(ctx.snippet);
  }
  ctx.snippet = snippet;

  // Only updated alongside the snippet: on a failed read the previous setting stays in force, for
  // the same reason the previous snippet does.
  ctx.injection_strategy =
      config.injection_strategy ? *config.injection_strategy : stable_injection_strategy(logger);
  ctx.inject_static_files =
      config.inject_static_files ? *config.inject_static_files : stable_inject_static_files(logger);

  ctx.application_id_tag = snippet->application_id != nullptr
                               ? std::format("application_id:{}", snippet->application_id)
                               : std::string();
  ctx.remote_config_tag =
      std::format("remote_config_used:{}", snippet->remote_configuration_id != nullptr);
}

}  // namespace datadog::rum
