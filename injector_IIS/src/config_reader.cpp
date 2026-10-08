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

#include <format>
#include <memory>
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

std::optional<std::string> read_string_property(IAppHostElement& element, std::wstring_view name,
                                                Logger& logger) {
  auto* property = get_property(element, name);
  if (property == nullptr) {
    logger.debug(std::format("Missing '{}' attribute", wstring_to_utf8(std::wstring(name))));
    return std::nullopt;
  }
  const auto defer_property = defer([&] { property->Release(); });
  auto value = get_property_value(*property);
  const auto defer_value = defer([&] { VariantClear(&value); });
  if (value.vt != VT_BSTR || value.bstrVal == nullptr) {
    logger.debug(std::format("'{}' is not a string", wstring_to_utf8(std::wstring(name))));
    return std::nullopt;
  }
  return wstring_to_utf8(std::wstring(value.bstrVal));
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

std::optional<std::unordered_map<std::string, std::string>> read_sdk_options(
    IAppHostElement& element, Logger& logger) {
  std::unordered_map<std::string, std::string> rum_sdk_opts;

  IAppHostElementCollection* col = nullptr;
  HRESULT const hr_col = element.get_Collection(&col);
  if (FAILED(hr_col)) {
    logger.error(std::format("Failed to read SDK options collection: {}",
                             std::system_category().message(hr_col)));
    return std::nullopt;
  }
  if (col == nullptr) {
    return rum_sdk_opts;
  }
  const auto defer_col = defer([&] { col->Release(); });

  DWORD n_elem = 0;
  HRESULT const hr_count = col->get_Count(&n_elem);
  if (FAILED(hr_count)) {
    logger.error(std::format("Failed to read SDK option count: {}",
                             std::system_category().message(hr_count)));
    return std::nullopt;
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

    auto name = read_string_property(*option_element, L"name", logger);
    if (!name.has_value()) {
      continue;
    }
    auto value = read_string_property(*option_element, L"value", logger);
    if (value.has_value()) {
      rum_sdk_opts.emplace(std::move(*name), std::move(*value));
    }
  }
  return rum_sdk_opts;
}

StableConfigReader g_stable_config_reader = stable_config_read;

}  // namespace

void set_stable_config_reader_for_testing(StableConfigReader reader) {
  g_stable_config_reader = reader != nullptr ? reader : stable_config_read;
}

::StableConfig* read_stable_config() {
  return g_stable_config_reader(k_stable_config_language, /* debug_logs */ false);
}

std::string make_config_json(std::optional<int> major_version,
                             const std::unordered_map<std::string, std::string>& opts,
                             Logger& logger) {
  rapidjson::Document doc;
  doc.SetObject();

  rapidjson::Document::AllocatorType& allocator = doc.GetAllocator();
  if (major_version.has_value()) {
    doc.AddMember("majorVersion", *major_version, allocator);
  }

  rapidjson::Value rum(rapidjson::kObjectType);
  for (const auto& [key, value] : opts) {
    if (key == "majorVersion") {
      continue;
    }
    rapidjson::Value encoded;
    if (key == "sessionSampleRate" || key == "sessionReplaySampleRate") {
      try {
        encoded.SetDouble(std::stod(value));
      } catch (const std::exception&) {
        logger.error(std::format("Invalid numeric value '{}' for config key '{}'", value, key));
        continue;
      }
    } else if (key == "trackResources" || key == "trackLongTasks" ||
               key == "trackUserInteractions") {
      encoded.SetBool(value == "true");
    } else if (key == "allowedTracingUrls") {
      encoded.SetArray();
      std::stringstream ss(value);
      std::string url;

      while (std::getline(ss, url, ',')) {
        auto start = url.find_first_not_of(" \t");
        if (start != std::string::npos) {
          auto end = url.find_last_not_of(" \t");
          url = url.substr(start, end - start + 1);

          encoded.PushBack(rapidjson::Value(url.c_str(), allocator).Move(), allocator);
        }
      }

      if (encoded.Empty()) {
        continue;
      }
    } else {
      encoded.SetString(value.c_str(), allocator);
    }
    rum.AddMember(rapidjson::Value(key.c_str(), allocator).Move(), encoded, allocator);
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

  return ServerConfig{read_enum_property(*cfg_root_elem, L"logLevel", LogLevel::Info,
                                         static_cast<int>(LogLevel::Debug), logger)};
}

ReadConfResult read_conf(const IHttpServer& server, PCWSTR cfg_path, Logger& logger) {
  auto* admin_manager = server.GetAdminManager();
  if (admin_manager == nullptr) {
    logger.debug("Admin manager is null, cannot read RUM config");
    return {ReadConfStatus::Error};
  }

  IAppHostElement* cfg_root_elem = nullptr;
  auto res = admin_manager->GetAdminSection((BSTR)L"system.webServer/datadogRum",
                                            const_cast<BSTR>(cfg_path), &cfg_root_elem);
  if (res != S_OK) {
    logger.debug(std::format("Failed to read datadogRum config section: {}",
                             std::system_category().message(res)));
    if (res == HRESULT_FROM_WIN32(ERROR_FILE_NOT_FOUND)) {
      return {ReadConfStatus::NotFound};
    }
    return {ReadConfStatus::Error};
  }
  if (cfg_root_elem == nullptr) {
    return {ReadConfStatus::Error};
  }

  const auto defer_root = defer([&] { cfg_root_elem->Release(); });

  IAppHostProperty* enabled_prop = get_property(*cfg_root_elem, L"enabled");
  if (enabled_prop == nullptr) {
    // get_property conflates "attribute missing" with COM failure; treat as
    // Error so a transient failure can't silently wipe injection.
    logger.debug("Failed to read 'enabled' property");
    return {ReadConfStatus::Error};
  }

  const auto defer_enabled_prop = defer([&] { enabled_prop->Release(); });

  auto enabled_val = get_property_value(*enabled_prop);
  const auto defer_enabled_val = defer([&] { VariantClear(&enabled_val); });
  if (enabled_val.vt != VT_BOOL) {
    logger.debug("Failed to read 'enabled' property value");
    return {ReadConfStatus::Error};
  }
  if (!enabled_val.boolVal) {
    logger.info("RUM injection disabled for this path");
    return {ReadConfStatus::Disabled};
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
      return {ReadConfStatus::Error};
    }
    if (is_default.boolVal == VARIANT_FALSE) {
      if (version_val.vt == VT_I4) {
        cfg_version = version_val.lVal;
      } else if (version_val.vt == VT_I2) {
        cfg_version = version_val.iVal;
      } else if (version_val.vt != VT_EMPTY) {
        logger.info(std::format("Unexpected version type (vt={}), leaving majorVersion unset",
                                version_val.vt));
      }
    }
  }

  auto const injection_strategy = read_enum_property(
      *cfg_root_elem, L"injectionStrategy", InjectionStrategy::HeadCloseTag,
      static_cast<int>(InjectionStrategy::HeadOpenTagWithCloseTagFallback), logger);

  auto rum_sdk_opts = read_sdk_options(*cfg_root_elem, logger);
  if (!rum_sdk_opts.has_value()) {
    return {ReadConfStatus::Error};
  }

  RumConfig config{cfg_version, std::move(*rum_sdk_opts), injection_strategy};
  if (config.sdk_options.empty()) {
    // IIS hands back the schema-defaulted section for every site once
    // datadogRum is registered in configSections, so "the section resolved" is
    // not evidence that anyone configured this site. Reporting NotFound is what
    // lets stable configuration apply to sites with no <datadogRum> element.
    logger.debug("datadogRum section carries no SDK options; treating as unconfigured");
    return {ReadConfStatus::NotFound, std::move(config)};
  }
  return {ReadConfStatus::Ok, std::move(config)};
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

  const auto& config = result.config;
  auto create_iis_snippet = [&] {
    const auto json = make_config_json(config.version.value_or(k_iis_default_major_version),
                                       config.sdk_options, logger);
    return snippet_create_from_json(json.c_str());
  };
  using SnippetPtr = std::unique_ptr<Snippet, decltype(&snippet_cleanup)>;
  SnippetPtr snippet(nullptr, snippet_cleanup);
  if (stable_config == nullptr) {
    snippet.reset(create_iis_snippet());
  } else {
    std::string overlay;
    if (config.version.has_value() || !config.sdk_options.empty()) {
      overlay = make_config_json(config.version, config.sdk_options, logger);
    }
    snippet.reset(
        stable_config_snippet_create(stable_config, overlay.empty() ? nullptr : overlay.c_str()));
    // Invalid stable values may fall back to a complete IIS configuration.
    // Successful stable opt-outs never enter this error path.
    if (snippet != nullptr && snippet->error_code >= 2 && snippet->error_code <= 6 &&
        result.status == ReadConfStatus::Ok) {
      SnippetPtr fallback(create_iis_snippet(), snippet_cleanup);
      if (fallback != nullptr && fallback->error_code == 0) {
        logger.info("Ignoring invalid stable RUM values; using IIS configuration");
        telemetry::increment(
            telemetry::config_invalid,
            telemetry::build_tags(std::format("error_code:{}", snippet->error_code),
                                  ctx.application_id_tag, ctx.remote_config_tag));
        snippet = std::move(fallback);
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
  ctx.snippet = snippet.release();
  ctx.application_id_tag = std::move(application_id_tag);
  ctx.remote_config_tag = std::move(remote_config_tag);
  ctx.injection_strategy = config.injection_strategy;
}

}  // namespace datadog::rum
