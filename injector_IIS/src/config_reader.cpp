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
#include <optional>
#include <rapidjson/document.h>
#include <rapidjson/stringbuffer.h>
#include <rapidjson/writer.h>
#include <set>
#include <shlwapi.h>
#include <sstream>
#include <string>
#include <string_view>
#include <xmllite.h>

namespace datadog::rum {
namespace {

// The injector supplies a process-wide baseline. IIS may override it at each application and
// location through the normal datadogRum section. Read at configuration time, outside DllMain.
// clang-tidy cannot prove that the standard container's implicit destructor is non-throwing.
// NOLINTNEXTLINE(bugprone-exception-escape)
struct BaselineConfig {
  std::optional<bool> enabled;
  std::optional<int> version;
  std::unordered_map<std::string, std::string> sdk_options;
};

std::optional<std::wstring> read_environment(LPCWSTR name) {
  SetLastError(ERROR_SUCCESS);
  const DWORD size = GetEnvironmentVariableW(name, nullptr, 0);
  if (size == 0) {
    if (GetLastError() == ERROR_ENVVAR_NOT_FOUND) return std::nullopt;
    return std::wstring();  // An explicitly empty variable still overrides the registry.
  }
  if (size > 32768) return std::wstring();

  std::wstring value(size, L'\0');
  const DWORD written = GetEnvironmentVariableW(name, value.data(), size);
  if (written >= size) return std::wstring();
  value.resize(written);
  return value;
}

std::optional<std::wstring> read_registry_baseline(Logger& logger) {
  constexpr auto* key = L"SOFTWARE\\Datadog\\IIS RUM";
  constexpr auto* name = L"DefaultConfig";
  constexpr DWORD flags = RRF_RT_REG_SZ | RRF_SUBKEY_WOW6464KEY;
  DWORD size = 0;
  DWORD type = 0;
  LONG result = RegGetValueW(HKEY_LOCAL_MACHINE, key, name, flags, &type, nullptr, &size);
  if (result == ERROR_FILE_NOT_FOUND || result == ERROR_PATH_NOT_FOUND) return std::nullopt;
  if (result != ERROR_SUCCESS || size == 0 || size > 65536 || size % sizeof(wchar_t) != 0) {
    logger.error("Cannot read IIS RUM registry baseline");
    return std::nullopt;
  }

  std::wstring value(size / sizeof(wchar_t), L'\0');
  result = RegGetValueW(HKEY_LOCAL_MACHINE, key, name, flags, &type, value.data(), &size);
  if (result != ERROR_SUCCESS) {
    logger.error("Cannot read IIS RUM registry baseline value");
    return std::nullopt;
  }
  const auto terminator = value.find(L'\0');
  if (terminator == std::wstring::npos) {
    logger.error("IIS RUM registry baseline is not terminated");
    return std::nullopt;
  }
  value.resize(terminator);
  return value;
}

BaselineConfig read_baseline(Logger& logger) {
  auto raw = read_environment(L"DD_IIS_RUM_CONFIG");
  if (!raw) raw = read_registry_baseline(logger);
  if (!raw) return {};

  std::string json = wstring_to_utf8(*raw);
  rapidjson::Document doc;
  doc.Parse(json.data(), json.size());
  if (doc.HasParseError() || !doc.IsObject()) {
    logger.error("Invalid IIS RUM baseline JSON");
    return {};
  }

  BaselineConfig baseline;
  if (doc.HasMember("enabled")) {
    if (!doc["enabled"].IsBool()) {
      logger.error("Invalid IIS RUM baseline enabled value");
      return {};
    }
    baseline.enabled = doc["enabled"].GetBool();
  }
  if (doc.HasMember("version")) {
    if (!doc["version"].IsInt() || doc["version"].GetInt() < 5 || doc["version"].GetInt() > 1000) {
      logger.error("Invalid IIS RUM baseline version value");
      return {};
    }
    baseline.version = doc["version"].GetInt();
  }
  if (!doc.HasMember("options")) return baseline;
  if (!doc["options"].IsObject()) {
    logger.error("Invalid IIS RUM baseline options value");
    return {};
  }
  for (auto item = doc["options"].MemberBegin(); item != doc["options"].MemberEnd(); ++item) {
    if (!item->name.IsString() || item->name.GetStringLength() == 0 || !item->value.IsString() ||
        item->value.GetStringLength() == 0) {
      logger.error("Invalid IIS RUM baseline option");
      return {};
    }
    baseline.sdk_options.insert_or_assign(
        std::string(item->name.GetString(), item->name.GetStringLength()),
        std::string(item->value.GetString(), item->value.GetStringLength()));
  }
  return baseline;
}

// A missing metadata implementation cannot prove that an IIS value came from the schema.
// In that case the effective IIS value retains precedence over the injector baseline.
bool is_schema_default(IAppHostProperty& property) {
  VARIANT value;
  VariantInit(&value);
  const HRESULT hr = property.GetMetadata((BSTR)L"isDefaultValue", &value);
  const bool result = SUCCEEDED(hr) && value.vt == VT_BOOL && value.boolVal == VARIANT_TRUE;
  VariantClear(&value);
  return result;
}

std::optional<std::wstring> element_metadata_string(IAppHostElement& element, LPCWSTR name) {
  VARIANT value;
  VariantInit(&value);
  const HRESULT hr = element.GetMetadata((BSTR)name, &value);
  if (SUCCEEDED(hr) && value.vt == VT_EMPTY) return std::wstring();
  if (FAILED(hr) || value.vt != VT_BSTR || value.bstrVal == nullptr) {
    VariantClear(&value);
    return std::nullopt;
  }
  std::wstring result(value.bstrVal, SysStringLen(value.bstrVal));
  VariantClear(&value);
  return result;
}

enum class DirectiveScan { Clean, Present, Unknown };
enum class BaselineOptionMerge { Merge, ManualDirective, ExternalSource, SourceUnavailable };

// Read IIS-reported configuration source files only. This catches explicit <clear> and <remove>
// directives that IIS has already consumed when it returns an effective option collection.
// Their intent must not be undone by inserting fallback options afterward.
DirectiveScan scan_option_directives(const std::wstring& file) {
  WIN32_FILE_ATTRIBUTE_DATA attributes;
  if (!GetFileAttributesExW(file.c_str(), GetFileExInfoStandard, &attributes) ||
      attributes.nFileSizeHigh != 0 || attributes.nFileSizeLow > 8 * 1024 * 1024) {
    return DirectiveScan::Unknown;
  }

  IStream* stream = nullptr;
  if (FAILED(SHCreateStreamOnFileEx(file.c_str(), STGM_READ | STGM_SHARE_DENY_NONE,
                                    FILE_ATTRIBUTE_NORMAL, FALSE, nullptr, &stream))) {
    return DirectiveScan::Unknown;
  }
  const auto release_stream = defer([&] { stream->Release(); });

  IXmlReader* reader = nullptr;
  if (FAILED(CreateXmlReader(__uuidof(IXmlReader), reinterpret_cast<void**>(&reader), nullptr))) {
    return DirectiveScan::Unknown;
  }
  const auto release_reader = defer([&] { reader->Release(); });
  if (FAILED(reader->SetProperty(XmlReaderProperty_DtdProcessing, DtdProcessing_Prohibit)) ||
      FAILED(reader->SetInput(stream))) {
    return DirectiveScan::Unknown;
  }

  bool in_rum = false;
  UINT rum_depth = 0;
  XmlNodeType node{};
  HRESULT hr = S_OK;
  while ((hr = reader->Read(&node)) == S_OK) {
    if (node != XmlNodeType_Element && node != XmlNodeType_EndElement) continue;
    const wchar_t* name = nullptr;
    UINT length = 0;
    if (FAILED(reader->GetLocalName(&name, &length))) return DirectiveScan::Unknown;
    const std::wstring_view local(name, length);
    UINT depth = 0;
    if (FAILED(reader->GetDepth(&depth))) return DirectiveScan::Unknown;

    if (node == XmlNodeType_Element) {
      if (in_rum && depth == rum_depth + 1 && (local == L"clear" || local == L"remove")) {
        return DirectiveScan::Present;
      }
      if (local == L"datadogRum") {
        in_rum = !reader->IsEmptyElement();
        if (in_rum) rum_depth = depth;
      }
    } else if (in_rum && depth == rum_depth && local == L"datadogRum") {
      in_rum = false;
    }
  }
  return hr == S_FALSE ? DirectiveScan::Clean : DirectiveScan::Unknown;
}

// Walk each configuration level to capture source files inherited from above the current path.
// Any uncertain source suppresses fallback options while leaving IIS's effective manual values
// untouched. Normal manual <option> additions still merge by key with the baseline.
BaselineOptionMerge can_merge_baseline_options(IAppHostAdminManager& manager, PCWSTR cfg_path) {
  constexpr std::wstring_view root = L"MACHINE/WEBROOT/APPHOST";
  if (cfg_path == nullptr) return BaselineOptionMerge::SourceUnavailable;
  std::wstring path(cfg_path);
  if (!std::wstring_view(path).starts_with(root) ||
      (path.size() > root.size() && path[root.size()] != L'/')) {
    return BaselineOptionMerge::SourceUnavailable;
  }

  BSTR section = SysAllocString(L"system.webServer/datadogRum");
  if (section == nullptr) return BaselineOptionMerge::SourceUnavailable;
  const auto release_section = defer([&] { SysFreeString(section); });
  std::set<std::wstring> sources;
  for (;;) {
    BSTR bstr_path = SysAllocStringLen(path.data(), static_cast<UINT>(path.size()));
    if (bstr_path == nullptr) return BaselineOptionMerge::SourceUnavailable;
    const auto release_path = defer([&] { SysFreeString(bstr_path); });
    IAppHostElement* element = nullptr;
    const HRESULT hr = manager.GetAdminSection(section, bstr_path, &element);
    if (FAILED(hr) || element == nullptr) return BaselineOptionMerge::SourceUnavailable;
    const auto release_element = defer([&] { element->Release(); });
    auto config_source = element_metadata_string(*element, L"configSource");
    auto child_source = element_metadata_string(*element, L"childSource");
    auto source_file = element_metadata_string(*element, L"deepestFileNameSet");
    if (!config_source || !child_source || !source_file) {
      return BaselineOptionMerge::SourceUnavailable;
    }
    if (!config_source->empty() || !child_source->empty())
      return BaselineOptionMerge::ExternalSource;
    if (!source_file->empty()) sources.insert(std::move(*source_file));
    if (path.size() == root.size()) break;
    path.resize(path.find_last_of(L'/'));
  }

  for (const auto& file : sources) {
    switch (scan_option_directives(file)) {
      case DirectiveScan::Present:
        return BaselineOptionMerge::ManualDirective;
      case DirectiveScan::Unknown:
        return BaselineOptionMerge::SourceUnavailable;
      case DirectiveScan::Clean:
        break;
    }
  }
  return BaselineOptionMerge::Merge;
}

std::optional<rapidjson::Value> parse_sdk_option(const std::string& key, const std::string& value,
                                                 rapidjson::Document::AllocatorType& allocator,
                                                 Logger& logger) {
  if (key == "majorVersion") return std::nullopt;
  if (key == "sessionSampleRate" || key == "sessionReplaySampleRate") {
    try {
      return rapidjson::Value(std::stod(value));
    } catch (const std::exception&) {
      logger.error(std::format("Invalid numeric value '{}' for config key '{}'", value, key));
      return std::nullopt;
    }
  }
  if (key == "trackResources" || key == "trackLongTasks" || key == "trackUserInteractions") {
    return rapidjson::Value(value == "true");
  }
  if (key != "allowedTracingUrls") return rapidjson::Value(value.c_str(), allocator);

  rapidjson::Value urls(rapidjson::kArrayType);
  std::stringstream input(value);
  std::string url;
  while (std::getline(input, url, ',')) {
    const auto start = url.find_first_not_of(" \t");
    if (start == std::string::npos) continue;
    const auto end = url.find_last_not_of(" \t");
    url = url.substr(start, end - start + 1);
    urls.PushBack(rapidjson::Value(url.c_str(), allocator).Move(), allocator);
  }
  if (urls.Empty()) return std::nullopt;
  return urls;
}

std::string make_overlay_json(const int version, bool include_version, std::optional<bool> enabled,
                              const std::unordered_map<std::string, std::string>& opts,
                              Logger& logger) {
  rapidjson::Document doc;
  doc.SetObject();

  rapidjson::Document::AllocatorType& allocator = doc.GetAllocator();
  if (include_version) doc.AddMember("majorVersion", version, allocator);
  if (enabled) doc.AddMember("enabled", *enabled, allocator);

  rapidjson::Value rum(rapidjson::kObjectType);
  for (const auto& [key, value] : opts) {
    auto option = parse_sdk_option(key, value, allocator, logger);
    if (option) {
      rum.AddMember(rapidjson::Value(key.c_str(), allocator).Move(), option->Move(), allocator);
    }
  }

  if (!opts.empty()) doc.AddMember("rum", rum, allocator);

  // Convert the document to a JSON string
  rapidjson::StringBuffer buffer;
  rapidjson::Writer<rapidjson::StringBuffer> writer(buffer);
  doc.Accept(writer);

  return {buffer.GetString(), buffer.GetSize()};
}

Snippet* make_snippet(const int version, const std::unordered_map<std::string, std::string>& opts,
                      Logger& logger) {
  const auto json = make_overlay_json(version, true, std::nullopt, opts, logger);
  return snippet_create_from_json(json.c_str());
}

std::optional<std::string> read_stable_path_override(LPCWSTR name) {
  const auto value = read_environment(name);
  if (!value) return std::nullopt;
  return wstring_to_utf8(*value);
}

// Rust stable configuration is immutable after loading. The process keeps the
// handle until worker exit, so all applications see one consistent snapshot.
StableConfig* stable_config_for_worker() {
  static StableConfig* config = [] {
    const auto local = read_stable_path_override(L"DD_IIS_RUM_STABLE_CONFIG_LOCAL");
    const auto fleet = read_stable_path_override(L"DD_IIS_RUM_STABLE_CONFIG_FLEET");
    return stable_config_read_with_paths("browser-sdk", false, local ? local->c_str() : nullptr,
                                         fleet ? fleet->c_str() : nullptr);
  }();
  return config;
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

// Merge the effective IIS collection over defaults. A collection read failure
// preserves the caller's last valid configuration; malformed entries are skipped.
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

    // The effective IIS collection already applies inheritance. Explicit application/location
    // option values take precedence over process defaults with the same name.
    rum_sdk_opts.insert_or_assign(std::move(opt_name), std::move(opt_value));
  }
  return true;
}

}  // namespace

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
  auto baseline = read_baseline(logger);
  auto* admin_manager = server.GetAdminManager();
  if (admin_manager == nullptr) {
    logger.debug("Admin manager is null, cannot read RUM config");
    return ReadConfResult::error();
  }

  IAppHostElement* cfg_root_elem = nullptr;
  auto res = admin_manager->GetAdminSection((BSTR)L"system.webServer/datadogRum",
                                            const_cast<BSTR>(cfg_path), &cfg_root_elem);
  if (res != S_OK) {
    logger.debug(std::format("datadogRum config section not found: {}",
                             std::system_category().message(res)));
    return ReadConfResult::absent();
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
  const bool enabled_is_default = is_schema_default(*enabled_prop);
  const bool enabled = baseline.enabled && enabled_is_default
                           ? *baseline.enabled
                           : enabled_val.boolVal != VARIANT_FALSE;
  if (!enabled) {
    logger.info("RUM injection disabled for this path");
    return ReadConfResult::absent();
  }

  int cfg_version = 6;
  bool version_overrides_stable = baseline.version.has_value();
  auto* version_prop = get_property(*cfg_root_elem, L"version");
  if (version_prop != nullptr) {
    const auto defer_version_prop = defer([&] { version_prop->Release(); });
    const bool version_is_default = is_schema_default(*version_prop);
    version_overrides_stable = version_overrides_stable || !version_is_default;
    auto version_val = get_property_value(*version_prop);
    const auto defer_version_val = defer([&] { VariantClear(&version_val); });
    if (version_val.vt == VT_I4) {
      cfg_version = version_val.lVal;
    } else if (version_val.vt == VT_I2) {
      cfg_version = version_val.iVal;
    } else if (version_val.vt != VT_EMPTY) {
      logger.info(
          std::format("Unexpected version type (vt={}), "
                      "using default version {}",
                      version_val.vt, cfg_version));
    }
    if (baseline.version && version_is_default) cfg_version = *baseline.version;
  } else if (baseline.version) {
    cfg_version = *baseline.version;
  }

  auto const injection_strategy = read_enum_property(
      *cfg_root_elem, L"injectionStrategy", InjectionStrategy::HeadCloseTag,
      static_cast<int>(InjectionStrategy::HeadOpenTagWithCloseTagFallback), logger);

  // Iterate on the SDK configuration
  std::unordered_map<std::string, std::string> rum_sdk_opts;
  const auto merge = can_merge_baseline_options(*admin_manager, cfg_path);
  switch (merge) {
    case BaselineOptionMerge::Merge:
      rum_sdk_opts = std::move(baseline.sdk_options);
      break;
    case BaselineOptionMerge::ManualDirective:
      logger.debug("IIS RUM clear/remove directive suppresses SDK option defaults");
      break;
    case BaselineOptionMerge::ExternalSource:
      logger.debug("IIS RUM external config source suppresses SDK option defaults");
      break;
    case BaselineOptionMerge::SourceUnavailable:
      logger.debug("IIS RUM config source cannot be verified; suppressing SDK option defaults");
      break;
  }

  if (!read_sdk_options(*cfg_root_elem, rum_sdk_opts, logger)) {
    return ReadConfResult::error();
  }

  return ReadConfResult::ok(RumConfig{cfg_version, std::move(rum_sdk_opts), injection_strategy,
                                      baseline.enabled.has_value() || !enabled_is_default,
                                      !enabled_is_default, version_overrides_stable,
                                      merge != BaselineOptionMerge::Merge});
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
    return;
  }

  auto& config = *result.config;
  const bool has_overlay = config.enabled_overrides_stable || config.version_overrides_stable ||
                           !config.sdk_options.empty();
  const auto overlay =
      has_overlay ? make_overlay_json(
                        config.version, config.version_overrides_stable,
                        config.enabled_overrides_stable ? std::optional<bool>(true) : std::nullopt,
                        config.sdk_options, logger)
                  : std::string();
  StableConfig* stable = stable_config_for_worker();
  Snippet* snippet = stable == nullptr ? nullptr
                                       : stable_config_snippet_create_with_options(
                                             stable, has_overlay ? overlay.c_str() : nullptr,
                                             config.suppress_stable_options);

  // On make_snippet failure, preserve the existing snippet/tags. Operators
  // prefer continued injection with the last-known-good config over going
  // dark on a broken deploy; the next valid reload replaces it.
  auto report_invalid = [&](std::string error_tag, bool clear, const std::string& message) {
    logger.error(message + (clear ? "; clearing previous config" : "; keeping previous config"));
    datadog::telemetry::counter::increment(
        telemetry::config_invalid,
        telemetry::build_tags(std::move(error_tag), ctx.application_id_tag, ctx.remote_config_tag));
    if (clear) clear_module_snippet(ctx);
  };

  if (snippet == nullptr) {
    report_invalid("error_code:alloc", true, "Failed to create RUM snippet");
    return;
  }
  if (snippet->error_code == 0 && snippet->rum_enabled == RUM_ENABLED_FALSE) {
    snippet_cleanup(snippet);
    clear_module_snippet(ctx);
    ctx.injection_strategy = config.injection_strategy;
    logger.info("RUM injection disabled by stable configuration");
    return;
  }
  if (snippet->error_code == 6 && config.enabled_explicit_iis) {
    // A broken stable source cannot establish its own opt-out. Only an explicit
    // IIS enablement with fully valid IIS/baseline credentials can bypass it.
    Snippet* manual = make_snippet(config.version, config.sdk_options, logger);
    if (manual != nullptr && manual->error_code == 0) {
      snippet_cleanup(snippet);
      snippet = manual;
    } else if (manual != nullptr) {
      snippet_cleanup(manual);
    }
  }
  if (snippet->error_code != 0) {
    const int error_code = snippet->error_code;
    const bool clear = config.suppress_stable_options || error_code == 6 || error_code == 10;
    report_invalid(std::format("error_code:{}", error_code), clear,
                   std::format("Failed to create RUM snippet (code {})", error_code));
    snippet_cleanup(snippet);
    return;
  }

  logger.info("Configuration validated");
  if (ctx.snippet != nullptr) {
    snippet_cleanup(ctx.snippet);
  }
  ctx.snippet = snippet;

  // Only updated alongside the snippet: on a failed read the previous setting stays in force, for
  // the same reason the previous snippet does.
  ctx.injection_strategy = config.injection_strategy;

  ctx.application_id_tag.clear();
  if (snippet->application_id != nullptr) {
    ctx.application_id_tag = std::format("application_id:{}", snippet->application_id);
  } else if (auto appIdIt = config.sdk_options.find("applicationId");
             appIdIt != config.sdk_options.end()) {
    ctx.application_id_tag = std::format("application_id:{}", appIdIt->second);
  }

  auto rcUsed = snippet->remote_configuration_id != nullptr ||
                config.sdk_options.count("remoteConfigurationId") > 0;
  ctx.remote_config_tag = std::format("remote_config_used:{}", rcUsed);
}

}  // namespace datadog::rum
