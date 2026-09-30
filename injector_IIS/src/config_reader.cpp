// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2024-Present Datadog, Inc.

#include "config_reader.h"
#include "defer.h"
#include "injectbrowsersdk.h"
#include "module_context.h"
#include "telemetry.h"

#include <cassert>
#include <datadog/telemetry/telemetry.h>
#include <format>
#include <rapidjson/document.h>
#include <rapidjson/stringbuffer.h>
#include <rapidjson/writer.h>
#include <sstream>
#include <string>
#include <utility>

namespace datadog::rum {
namespace {

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

// Reads an `enum`-typed attribute, which IIS hands back as an integer. Returns `fallback` when the
// attribute is missing, of an unexpected type, or out of range: an unreadable knob must not take
// injection down, and the fallback is always the historical behaviour.
template <typename Enum>
Enum read_enum_property(IAppHostElement& element, std::wstring_view name, Enum fallback,
                        int max_value, Logger& logger) {
  auto* property = get_property(element, name);
  if (property == nullptr) {
    return fallback;
  }
  const auto defer_property = defer([&] { property->Release(); });

  auto value = get_property_value(*property);
  const auto defer_value = defer([&] { VariantClear(&value); });
  if (value.vt != VT_I4 && value.vt != VT_I2) {
    if (value.vt != VT_EMPTY) {
      logger.info(std::format("Unexpected '{}' type (vt={}), using the default",
                              wstring_to_utf8(std::wstring(name)), value.vt));
    }
    return fallback;
  }

  int const raw = (value.vt == VT_I4) ? value.lVal : value.iVal;
  if (raw < 0 || raw > max_value) {
    logger.info(std::format("Out of range '{}' value ({}), using the default",
                            wstring_to_utf8(std::wstring(name)), raw));
    return fallback;
  }
  return static_cast<Enum>(raw);
}

// Test-only override of the stable-config file paths; see
// set_stable_config_paths_for_testing.
const char* g_stable_config_local_path = nullptr;
const char* g_stable_config_fleet_path = nullptr;

}  // namespace

// NOLINTNEXTLINE(bugprone-easily-swappable-parameters)
void set_stable_config_paths_for_testing(const char* local_path, const char* fleet_path) {
  g_stable_config_local_path = local_path;
  g_stable_config_fleet_path = fleet_path;
}

std::pair<const char*, const char*> stable_config_paths() {
  return {g_stable_config_local_path, g_stable_config_fleet_path};
}

std::string make_config_json(std::optional<int> major_version, std::optional<bool> enabled,
                             const std::unordered_map<std::string, std::string>& opts,
                             Logger& logger) {
  rapidjson::Document doc;
  doc.SetObject();

  rapidjson::Document::AllocatorType& allocator = doc.GetAllocator();
  if (major_version.has_value()) {
    doc.AddMember("majorVersion", *major_version, allocator);
  }
  if (enabled.has_value()) {
    doc.AddMember("enabled", *enabled, allocator);
  }

  rapidjson::Value rum(rapidjson::kObjectType);
  for (const auto& [key, value] : opts) {
    if (key == "majorVersion") {
      continue;
    }
    if (key == "sessionSampleRate" || key == "sessionReplaySampleRate") {
      try {
        rum.AddMember(rapidjson::Value(key.c_str(), allocator).Move(),
                      rapidjson::Value(std::stod(value)).Move(), allocator);
      } catch (const std::exception&) {
        logger.error(std::format("Invalid numeric value '{}' for config key '{}'", value, key));
        continue;
      }
    } else if (key == "trackResources" || key == "trackLongTasks" ||
               key == "trackUserInteractions") {
      auto b = (value == "true");
      rum.AddMember(rapidjson::Value(key.c_str(), allocator).Move(), rapidjson::Value(b).Move(),
                    allocator);
    } else if (key == "allowedTracingUrls") {
      rapidjson::Value urlArray(rapidjson::kArrayType);
      std::stringstream ss(value);
      std::string url;

      while (std::getline(ss, url, ',')) {
        auto start = url.find_first_not_of(" \t");
        if (start != std::string::npos) {
          auto end = url.find_last_not_of(" \t");
          url = url.substr(start, end - start + 1);

          urlArray.PushBack(rapidjson::Value(url.c_str(), allocator).Move(), allocator);
        }
      }

      if (!urlArray.Empty()) {
        rum.AddMember(rapidjson::Value(key.c_str(), allocator).Move(), urlArray.Move(), allocator);
      }
    } else {
      rum.AddMember(rapidjson::Value(key.c_str(), allocator).Move(),
                    rapidjson::Value(value.c_str(), allocator).Move(), allocator);
    }
  }

  // Always present, even when empty: the overlay's `rum` member is what carries
  // the per-site SDK options, and an absent one is indistinguishable from an
  // empty one to the parser.
  doc.AddMember("rum", rum, allocator);

  // Convert the document to a JSON string
  rapidjson::StringBuffer buffer;
  rapidjson::Writer<rapidjson::StringBuffer> writer(buffer);
  doc.Accept(writer);

  return buffer.GetString();
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
  ServerConfig default_config{LogLevel::Info};
  auto* admin_manager = server.GetAdminManager();

  if (admin_manager == nullptr) {
    logger.debug("Admin manager is null, using default server config");
    return default_config;
  }

  IAppHostElement* cfg_root_elem = nullptr;
  auto res = admin_manager->GetAdminSection((BSTR)L"system.applicationHost/datadogRumServer",
                                            (BSTR)L"MACHINE/WEBROOT/APPHOST", &cfg_root_elem);

  if (res != S_OK) {
    // Section may legitimately not exist if the user hasn't configured it.
    logger.debug("datadogRumServer section not found, using default config");
    return default_config;
  }

  const auto defer_root = defer([&] { cfg_root_elem->Release(); });

  LogLevel log_level = LogLevel::Info;
  auto* log_level_prop = get_property(*cfg_root_elem, L"logLevel");
  if (log_level_prop != nullptr) {
    const auto defer_log_level_prop = defer([&] { log_level_prop->Release(); });
    auto log_level_val = get_property_value(*log_level_prop);
    const auto defer_log_level_val = defer([&] { VariantClear(&log_level_val); });
    if (log_level_val.vt == VT_I4 || log_level_val.vt == VT_I2) {
      int const log_level_int =
          (log_level_val.vt == VT_I4) ? log_level_val.lVal : log_level_val.iVal;
      if (log_level_int <= static_cast<int>(LogLevel::Debug) &&
          log_level_int >= static_cast<int>(LogLevel::Error)) {
        log_level = static_cast<LogLevel>(log_level_int);
      }
    } else {
      logger.info(
          std::format("Unexpected logLevel type (vt={}), "
                      "using default Info level",
                      log_level_val.vt));
    }
  }

  return ServerConfig{log_level};
}

ReadConfResult read_conf(const IHttpServer& server, PCWSTR cfg_path, Logger& logger) {
  auto* admin_manager = server.GetAdminManager();
  if (admin_manager == nullptr) {
    logger.debug("Admin manager is null, cannot read RUM config");
    return ReadConfResult::error();
  }

  IAppHostElement* cfg_root_elem = nullptr;
  auto res = admin_manager->GetAdminSection((BSTR)L"system.webServer/datadogRum",
                                            const_cast<BSTR>(cfg_path), &cfg_root_elem);
  if (res != S_OK) {
    logger.debug(std::format("Failed to read datadogRum config section: {}",
                             std::system_category().message(res)));
    if (res == HRESULT_FROM_WIN32(ERROR_FILE_NOT_FOUND)) {
      return ReadConfResult::not_found();
    }
    return ReadConfResult::error();
  }
  if (cfg_root_elem == nullptr) {
    return ReadConfResult::error();
  }

  const auto defer_root = defer([&] { cfg_root_elem->Release(); });

  IAppHostProperty* enabled_prop = get_property(*cfg_root_elem, L"enabled");
  if (enabled_prop == nullptr) {
    // get_property conflates "attribute missing" with COM failure; treat as
    // Error so a transient failure can't silently wipe injection.
    logger.debug("Failed to read 'enabled' property");
    return ReadConfResult::error();
  }

  const auto defer_enabled_prop = defer([&] { enabled_prop->Release(); });

  auto enabled_val = get_property_value(*enabled_prop);
  const auto defer_enabled_val = defer([&] { VariantClear(&enabled_val); });
  if (enabled_val.vt != VT_BOOL) {
    logger.debug("Failed to read 'enabled' property value");
    return ReadConfResult::error();
  }
  if (!enabled_val.boolVal) {
    logger.info("RUM injection disabled for this path");
    return ReadConfResult::disabled();
  }

  // Left unset when the section carries no usable version, so the value can
  // come from stable configuration instead of being pinned here.
  std::optional<int> cfg_version;
  auto* version_prop = get_property(*cfg_root_elem, L"version");
  if (version_prop != nullptr) {
    const auto defer_version_prop = defer([&] { version_prop->Release(); });
    auto version_val = get_property_value(*version_prop);
    const auto defer_version_val = defer([&] { VariantClear(&version_val); });
    // A schema default must not override a stable-config major version. IIS exposes
    // the same isDefaultValue metadata used by ConfigurationAttribute.
    VARIANT is_default;
    VariantInit(&is_default);
    const auto defer_default = defer([&] { VariantClear(&is_default); });
    const HRESULT hr_default = version_prop->GetMetadata((BSTR)L"isDefaultValue", &is_default);
    if (FAILED(hr_default) || is_default.vt != VT_BOOL) {
      logger.debug("Failed to read version default metadata");
      return ReadConfResult::error();
    }
    if (is_default.boolVal != VARIANT_FALSE) {
      // Leave majorVersion unset so stable configuration can supply it.
    } else if (version_val.vt == VT_I4) {
      cfg_version = version_val.lVal;
    } else if (version_val.vt == VT_I2) {
      cfg_version = version_val.iVal;
    } else if (version_val.vt != VT_EMPTY) {
      logger.info(std::format("Unexpected version type (vt={}), leaving majorVersion unset",
                              version_val.vt));
    }
  }

  auto const injection_strategy = read_enum_property(
      *cfg_root_elem, L"injectionStrategy", InjectionStrategy::HeadCloseTag,
      static_cast<int>(InjectionStrategy::HeadOpenTagWithCloseTagFallback), logger);

  // Iterate on the SDK configuration
  std::unordered_map<std::string, std::string> rum_sdk_opts;

  IAppHostElementCollection* col = nullptr;
  HRESULT const hr_col = cfg_root_elem->get_Collection(&col);
  if (FAILED(hr_col)) {
    logger.error(std::format("Failed to read SDK options collection: {}",
                             std::system_category().message(hr_col)));
    return ReadConfResult::error();
  }
  if (col != nullptr) {
    const auto defer_col = defer([&] { col->Release(); });

    DWORD n_elem = 0;
    HRESULT const hr_count = col->get_Count(&n_elem);
    if (FAILED(hr_count)) {
      logger.error(std::format("Failed to read SDK option count: {}",
                               std::system_category().message(hr_count)));
      return ReadConfResult::error();
    }

    VARIANT item_idx;
    item_idx.vt = VT_UI4;

    for (DWORD i = 0; i < n_elem; ++i) {
      item_idx.ulVal = i;

      IAppHostElement* option_element = nullptr;
      HRESULT const hr_item = col->get_Item(item_idx, &option_element);
      if (FAILED(hr_item) || option_element == nullptr) {
        logger.debug(
            std::format("Skipping config option at index {}: "
                        "failed to read element",
                        i));
        continue;
      }

      const auto option_element_guard = defer([&] { option_element->Release(); });

      auto* name_property = get_property(*option_element, L"name");
      if (name_property == nullptr) {
        logger.debug(
            std::format("Skipping config option at index {}: "
                        "missing 'name' attribute",
                        i));
        continue;
      }
      const auto defer_name_property = defer([&] { name_property->Release(); });
      auto name_val = get_property_value(*name_property);
      const auto defer_name_val = defer([&] { VariantClear(&name_val); });
      if (name_val.vt != VT_BSTR || name_val.bstrVal == nullptr) {
        logger.debug(
            std::format("Skipping config option at index {}: "
                        "'name' is not a string",
                        i));
        continue;
      }
      std::string opt_name = wstring_to_utf8(std::wstring(name_val.bstrVal));

      auto* value_property = get_property(*option_element, L"value");
      if (value_property == nullptr) {
        logger.debug(
            std::format("Skipping config option '{}': "
                        "missing 'value' attribute",
                        opt_name));
        continue;
      }
      const auto defer_value_property = defer([&] { value_property->Release(); });
      auto val_val = get_property_value(*value_property);
      const auto defer_val_val = defer([&] { VariantClear(&val_val); });
      if (val_val.vt != VT_BSTR || val_val.bstrVal == nullptr) {
        logger.debug(
            std::format("Skipping config option '{}': "
                        "'value' is not a string",
                        opt_name));
        continue;
      }
      std::string opt_value = wstring_to_utf8(std::wstring(val_val.bstrVal));

      rum_sdk_opts.emplace(std::move(opt_name), std::move(opt_value));
    }
  }

  RumConfig config{cfg_version, std::move(rum_sdk_opts), injection_strategy};
  if (config.empty()) {
    // IIS hands back the schema-defaulted section for every site once
    // datadogRum is registered in configSections, so "the section resolved" is
    // not evidence that anyone configured this site. Reporting NotFound is what
    // lets stable configuration apply to sites with no <datadogRum> element.
    logger.debug("datadogRum section carries no SDK options; treating as unconfigured");
    return ReadConfResult::not_found(std::move(config));
  }
  return ReadConfResult::ok(std::move(config));
}

void update_module_context(ModuleContext& ctx, const IHttpServer& server, PCWSTR config_path,
                           Logger& logger, const ::StableConfig* stable_config) {
  auto disable_injection = [&] {
    if (ctx.snippet != nullptr) {
      snippet_cleanup(ctx.snippet);
      ctx.snippet = nullptr;
    }
    ctx.application_id_tag.clear();
    ctx.remote_config_tag.clear();
    ctx.injection_strategy = InjectionStrategy::HeadCloseTag;
  };

  auto result = read_conf(server, config_path, logger);
  switch (result.status) {
    case ReadConfStatus::Error:
      logger.info("Keeping existing config after transient read error");
      return;
    case ReadConfStatus::Disabled:
      // An explicit enabled="false" outranks stable configuration.
      disable_injection();
      return;
    case ReadConfStatus::NotFound:
      if (stable_config == nullptr) {
        disable_injection();
        return;
      }
      break;
    case ReadConfStatus::Ok:
      break;
  }

  // Rendered outside the branches so the C string stays alive across the call.
  std::string config_json;
  Snippet* snippet = nullptr;
  if (stable_config == nullptr) {
    // No usable stable configuration: the IIS section is the whole story, so
    // supply the schema's majorVersion rather than leaving it to the library's
    // own default.
    const auto& config = *result.config;
    config_json = make_config_json(config.version.value_or(k_iis_default_major_version),
                                   std::nullopt, config.sdk_options, logger);
    snippet = snippet_create_from_json(config_json.c_str());
  } else {
    const char* overlay = nullptr;
    if (result.config.has_value() &&
        (result.config->version.has_value() || !result.config->sdk_options.empty())) {
      const auto& config = *result.config;
      // `enabled` is deliberately never emitted: IIS's enabled="false" is
      // already handled as Disabled above, and emitting enabled=true here would
      // override a DD_RUM_ENABLED=false coming from stable configuration.
      config_json = make_config_json(config.version, std::nullopt, config.sdk_options, logger);
      overlay = config_json.c_str();
    }
    snippet = stable_config_snippet_create(stable_config, overlay);
    // A parsed file can still carry invalid RUM values. A complete, valid IIS
    // configuration remains usable in that case, just as when the file fails
    // to parse. Successful stable opt-outs never enter this error path.
    if (snippet != nullptr && snippet->error_code >= 2 && snippet->error_code <= 6 &&
        result.status == ReadConfStatus::Ok) {
      const auto& config = *result.config;
      config_json = make_config_json(config.version.value_or(k_iis_default_major_version),
                                     std::nullopt, config.sdk_options, logger);
      Snippet* fallback = snippet_create_from_json(config_json.c_str());
      if (fallback != nullptr && fallback->error_code == 0) {
        logger.info("Ignoring invalid stable RUM values; using IIS configuration");
        telemetry::increment(
            telemetry::config_invalid,
            telemetry::build_tags(std::format("error_code:{}", snippet->error_code),
                                  ctx.application_id_tag, ctx.remote_config_tag));
        snippet_cleanup(snippet);
        snippet = fallback;
      } else {
        snippet_cleanup(fallback);
      }
    }
  }

  // On snippet-creation failure, preserve the existing snippet/tags. Operators
  // prefer continued injection with the last-known-good config over going
  // dark on a broken deploy; the next valid reload replaces it.
  auto report_invalid = [&](std::string error_tag, const std::string& message) {
    logger.error(message + "; keeping previous config");
    telemetry::increment(
        telemetry::config_invalid,
        telemetry::build_tags(std::move(error_tag), ctx.application_id_tag, ctx.remote_config_tag));
  };

  if (snippet == nullptr) {
    report_invalid("error_code:alloc", "Failed to create RUM snippet");
    return;
  }
  const auto defer_snippet = defer([&] {
    if (snippet != nullptr) {
      snippet_cleanup(snippet);
    }
  });

  if (snippet->error_code == 10) {
    // Nothing configured in IIS, nothing in application_monitoring.yaml. The
    // ordinary state of a host that doesn't use RUM injection, so stay quiet
    // and emit no telemetry.
    logger.debug("No RUM configuration found in IIS config or stable configuration");
    disable_injection();
    return;
  }
  if (snippet->error_code != 0) {
    report_invalid(std::format("error_code:{}", snippet->error_code),
                   std::format("Failed to create RUM snippet: {}", snippet->error_message));
    return;
  }
  if (snippet->rum_enabled == RUM_ENABLED_FALSE || snippet->length == 0) {
    logger.info("RUM injection disabled by stable configuration");
    disable_injection();
    return;
  }

  // Read the resolved ids off the snippet before anything can free it: they are
  // the only place values sourced from stable configuration are visible.
  std::string application_id_tag;
  if (snippet->application_id != nullptr) {
    application_id_tag = std::format("application_id:{}", snippet->application_id);
  }
  auto remote_config_tag =
      std::format("remote_config_used:{}", snippet->remote_configuration_id != nullptr);

  logger.info("Configuration validated");
  if (ctx.snippet != nullptr) {
    snippet_cleanup(ctx.snippet);
  }
  ctx.snippet = snippet;
  snippet = nullptr;  // ownership moved to ctx; keep defer_snippet from freeing it
  ctx.application_id_tag = std::move(application_id_tag);
  ctx.remote_config_tag = std::move(remote_config_tag);
  ctx.injection_strategy = result.config.has_value() ? result.config->injection_strategy
                                                     : InjectionStrategy::HeadCloseTag;
}

}  // namespace datadog::rum
