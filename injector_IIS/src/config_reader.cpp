// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2024-Present Datadog, Inc.

#include "config_reader.h"
#include "defer.h"
#include "injectbrowsersdk.h"
#include "module_context.h"

#include <cassert>
#include <format>
#include <rapidjson/document.h>
#include <rapidjson/stringbuffer.h>
#include <rapidjson/writer.h>
#include <sstream>
#include <string>

namespace datadog::rum {
namespace {

Snippet *
make_snippet(const int version,
             const std::unordered_map<std::string, std::string> &opts,
             Logger &logger) {
  rapidjson::Document doc;
  doc.SetObject();

  rapidjson::Document::AllocatorType &allocator = doc.GetAllocator();
  doc.AddMember("majorVersion", version, allocator);

  rapidjson::Value rum(rapidjson::kObjectType);
  for (const auto &[key, value] : opts) {
    if (key == "majorVersion") {
      continue;
    } else if (key == "sessionSampleRate" || key == "sessionReplaySampleRate") {
      try {
        rum.AddMember(rapidjson::Value(key.c_str(), allocator).Move(),
                      rapidjson::Value(std::stod(value)).Move(), allocator);
      } catch (const std::exception &) {
        logger.error(std::format(
            "Invalid numeric value '{}' for config key '{}'", value, key));
        continue;
      }
    } else if (key == "trackResources" || key == "trackLongTasks" ||
               key == "trackUserInteractions") {
      auto b = (value == "true" ? true : false);
      rum.AddMember(rapidjson::Value(key.c_str(), allocator).Move(),
                    rapidjson::Value(b).Move(), allocator);
    } else if (key == "allowedTracingUrls") {
      rapidjson::Value urlArray(rapidjson::kArrayType);
      std::stringstream ss(value);
      std::string url;

      while (std::getline(ss, url, ',')) {
        auto start = url.find_first_not_of(" \t");
        if (start != std::string::npos) {
          auto end = url.find_last_not_of(" \t");
          url = url.substr(start, end - start + 1);

          urlArray.PushBack(rapidjson::Value(url.c_str(), allocator).Move(),
                            allocator);
        }
      }

      if (!urlArray.Empty()) {
        rum.AddMember(rapidjson::Value(key.c_str(), allocator).Move(),
                      urlArray.Move(), allocator);
      }
    } else {
      rum.AddMember(rapidjson::Value(key.c_str(), allocator).Move(),
                    rapidjson::Value(value.c_str(), allocator).Move(),
                    allocator);
    }
  }

  doc.AddMember("rum", rum, allocator);

  // Convert the document to a JSON string
  rapidjson::StringBuffer buffer;
  rapidjson::Writer<rapidjson::StringBuffer> writer(buffer);
  doc.Accept(writer);

  return snippet_create_from_json(buffer.GetString());
}

IAppHostProperty *get_property(IAppHostElement &xmlElement,
                               std::wstring_view name) {
  IAppHostProperty *property = nullptr;
  if (xmlElement.GetPropertyByName((BSTR)name.data(), &property) != S_OK) {
    return nullptr;
  }

  return property;
}

// Returns a VT_EMPTY variant if the COM call fails or the property has no
// value. Callers must check v.vt before accessing union members, and must
// call VariantClear on the returned VARIANT when done.
VARIANT get_property_value(IAppHostProperty &property) {
  VARIANT v;
  VariantInit(&v);
  HRESULT hr = property.get_Value(&v);
  if (FAILED(hr)) {
    VariantClear(&v);
  }
  return v;
}

} // namespace

std::string wstring_to_utf8(const std::wstring &wstr) {
  if (wstr.empty())
    return std::string();
  const int size_needed = WideCharToMultiByte(
      CP_UTF8, 0, wstr.data(), (int)wstr.size(), nullptr, 0, nullptr, nullptr);
  if (size_needed <= 0)
    return std::string();
  std::string result(size_needed, 0);
  int written = WideCharToMultiByte(CP_UTF8, 0, wstr.data(), (int)wstr.size(),
                                    result.data(), size_needed, nullptr, nullptr);
  if (written <= 0)
    return std::string();
  return result;
}

std::optional<ServerConfig> read_server_conf(const IHttpServer &server,
                                             Logger &logger) {
  ServerConfig default_config{LogLevel::Info};
  auto admin_manager = server.GetAdminManager();

  if (admin_manager == nullptr) {
    logger.debug("Admin manager is null, using default server config");
    return default_config;
  }

  IAppHostElement *cfg_root_elem;
  auto res = admin_manager->GetAdminSection(
      (BSTR)L"system.applicationHost/datadogRumServer",
      (BSTR)L"MACHINE/WEBROOT/APPHOST", &cfg_root_elem);

  if (res != S_OK) {
    // Section may legitimately not exist if the user hasn't configured it.
    logger.debug("datadogRumServer section not found, using default config");
    return default_config;
  }

  const auto defer_root = defer([&] { cfg_root_elem->Release(); });

  LogLevel log_level = LogLevel::Info;
  auto *log_level_prop = get_property(*cfg_root_elem, L"logLevel");
  if (log_level_prop != nullptr) {
    const auto defer_log_level_prop = defer([&] { log_level_prop->Release(); });
    auto log_level_val = get_property_value(*log_level_prop);
    const auto defer_log_level_val = defer([&] { VariantClear(&log_level_val); });
    if (log_level_val.vt == VT_I4 || log_level_val.vt == VT_I2) {
      int log_level_int =
          (log_level_val.vt == VT_I4) ? log_level_val.lVal : log_level_val.iVal;
      if (log_level_int <= static_cast<int>(LogLevel::Debug) &&
          log_level_int >= static_cast<int>(LogLevel::Error)) {
        log_level = static_cast<LogLevel>(log_level_int);
      }
    } else {
      logger.info(std::format("Unexpected logLevel type (vt={}), "
                              "using default Info level", log_level_val.vt));
    }
  }

  return ServerConfig{log_level};
}

ReadConfResult read_conf(const IHttpServer &server, PCWSTR cfg_path,
                         Logger &logger) {
  auto admin_manager = server.GetAdminManager();
  if (admin_manager == nullptr) {
    logger.debug("Admin manager is null, cannot read RUM config");
    return {ReadConfStatus::Error, std::nullopt};
  }

  IAppHostElement *cfg_root_elem;
  auto res = admin_manager->GetAdminSection(
      (BSTR)L"system.webServer/datadogRum", const_cast<BSTR>(cfg_path),
      &cfg_root_elem);
  if (res != S_OK) {
    logger.debug(std::format("datadogRum config section not found: {}",
                             std::system_category().message(res)));
    return {ReadConfStatus::Absent, std::nullopt};
  }

  const auto defer_root = defer([&] { cfg_root_elem->Release(); });

  IAppHostProperty *enabled_prop = get_property(*cfg_root_elem, L"enabled");
  if (enabled_prop == nullptr) {
    // missing enabled attribute -> invalid format -> disabled
    return {ReadConfStatus::Absent, std::nullopt};
  }

  const auto defer_enabled_prop = defer([&] { enabled_prop->Release(); });

  auto enabled_val = get_property_value(*enabled_prop);
  const auto defer_enabled_val = defer([&] { VariantClear(&enabled_val); });
  if (enabled_val.vt != VT_BOOL) {
    logger.debug("Failed to read 'enabled' property value");
    return {ReadConfStatus::Error, std::nullopt};
  }
  if (!enabled_val.boolVal) {
    logger.info("RUM injection disabled for this path");
    return {ReadConfStatus::Absent, std::nullopt};
  }

  int cfg_version = 6;
  auto *version_prop = get_property(*cfg_root_elem, L"version");
  if (version_prop != nullptr) {
    const auto defer_version_prop = defer([&] { version_prop->Release(); });
    auto version_val = get_property_value(*version_prop);
    const auto defer_version_val = defer([&] { VariantClear(&version_val); });
    if (version_val.vt == VT_I4) {
      cfg_version = version_val.lVal;
    } else if (version_val.vt == VT_I2) {
      cfg_version = version_val.iVal;
    } else if (version_val.vt != VT_EMPTY) {
      logger.info(std::format("Unexpected version type (vt={}), "
                              "using default version {}", version_val.vt, cfg_version));
    }
  }

  // Iterate on the SDK configuration
  std::unordered_map<std::string, std::string> rum_sdk_opts;

  IAppHostElementCollection *col = nullptr;
  HRESULT hr_col = cfg_root_elem->get_Collection(&col);
  if (FAILED(hr_col)) {
    logger.error(std::format("Failed to read SDK options collection: {}",
                             std::system_category().message(hr_col)));
    return {ReadConfStatus::Error, std::nullopt};
  }
  if (col != nullptr) {
    const auto defer_col = defer([&] { col->Release(); });

    DWORD n_elem = 0;
    HRESULT hr_count = col->get_Count(&n_elem);
    if (FAILED(hr_count)) {
      logger.error(std::format("Failed to read SDK option count: {}",
                               std::system_category().message(hr_count)));
      return {ReadConfStatus::Error, std::nullopt};
    }

    VARIANT item_idx;
    item_idx.vt = VT_UI4;

    for (DWORD i = 0; i < n_elem; ++i) {
      item_idx.ulVal = i;

      IAppHostElement *option_element = nullptr;
      HRESULT hr_item = col->get_Item(item_idx, &option_element);
      if (FAILED(hr_item) || option_element == nullptr) {
        logger.debug(std::format("Skipping config option at index {}: "
                                 "failed to read element", i));
        continue;
      }

      const auto option_element_guard =
          defer([&] { option_element->Release(); });

      auto name_property = get_property(*option_element, L"name");
      if (name_property == nullptr) {
        logger.debug(std::format("Skipping config option at index {}: "
                                 "missing 'name' attribute", i));
        continue;
      }
      const auto defer_name_property = defer([&] { name_property->Release(); });
      auto name_val = get_property_value(*name_property);
      const auto defer_name_val = defer([&] { VariantClear(&name_val); });
      if (name_val.vt != VT_BSTR || name_val.bstrVal == nullptr) {
        logger.debug(std::format("Skipping config option at index {}: "
                                 "'name' is not a string", i));
        continue;
      }
      std::string opt_name = wstring_to_utf8(std::wstring(name_val.bstrVal));

      auto value_property = get_property(*option_element, L"value");
      if (value_property == nullptr) {
        logger.debug(std::format("Skipping config option '{}': "
                                 "missing 'value' attribute", opt_name));
        continue;
      }
      const auto defer_value_property =
          defer([&] { value_property->Release(); });
      auto val_val = get_property_value(*value_property);
      const auto defer_val_val = defer([&] { VariantClear(&val_val); });
      if (val_val.vt != VT_BSTR || val_val.bstrVal == nullptr) {
        logger.debug(std::format("Skipping config option '{}': "
                                 "'value' is not a string", opt_name));
        continue;
      }
      std::string opt_value = wstring_to_utf8(std::wstring(val_val.bstrVal));

      rum_sdk_opts.emplace(std::move(opt_name), std::move(opt_value));
    }
  }

  return {ReadConfStatus::Ok, RumConfig{cfg_version, std::move(rum_sdk_opts)}};
}

void update_module_context(ModuleContext &ctx, const IHttpServer &server,
                           PCWSTR config_path, Logger &logger) {

  auto result = read_conf(server, config_path, logger);
  if (result.status == ReadConfStatus::Error) {
    // Transient COM error — keep existing config so a subsequent
    // OnGlobalConfigurationChange can retry and self-heal.
    logger.info("Keeping existing config after transient read error");
    return;
  }
  if (result.status == ReadConfStatus::Absent) {
    // Config definitively missing or disabled — clear snippet.
    if (ctx.snippet != nullptr) {
      snippet_cleanup(ctx.snippet);
      ctx.snippet = nullptr;
    }
    ctx.application_id_tag.clear();
    ctx.remote_config_tag.clear();
    return;
  }

  auto &config = *result.config;
  Snippet *snippet =
      make_snippet(config.version, config.sdk_options, logger);

  if (snippet == nullptr) {
    logger.error("Failed to create RUM snippet");
    if (ctx.snippet != nullptr) {
      snippet_cleanup(ctx.snippet);
      ctx.snippet = nullptr;
    }
    ctx.application_id_tag.clear();
    ctx.remote_config_tag.clear();
    return;
  } else if (snippet->error_code != 0) {
    logger.error(std::format("Failed to create RUM snippet: {}",
                              snippet->error_message));
    snippet_cleanup(snippet);
    if (ctx.snippet != nullptr) {
      snippet_cleanup(ctx.snippet);
      ctx.snippet = nullptr;
    }
    ctx.application_id_tag.clear();
    ctx.remote_config_tag.clear();
    return;
  }

  logger.info("Configuration validated");
  if (ctx.snippet != nullptr) {
    snippet_cleanup(ctx.snippet);
  }
  ctx.snippet = snippet;

  ctx.application_id_tag.clear();
  if (auto appIdIt = config.sdk_options.find("applicationId");
      appIdIt != config.sdk_options.end()) {
    ctx.application_id_tag = std::format("application_id:{}", appIdIt->second);
  }

  auto rcUsed = config.sdk_options.count("remoteConfigurationId") > 0;
  ctx.remote_config_tag = std::format("remote_config_used:{}", rcUsed);
}

} // namespace datadog::rum
