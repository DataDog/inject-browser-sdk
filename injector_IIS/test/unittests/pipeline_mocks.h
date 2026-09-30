/*
 * Unless explicitly stated otherwise all files in this repository are licensed
 * under the Apache 2.0 License. This product includes software developed at
 * Datadog (https://www.datadoghq.com/).
 *
 * Copyright 2024-Present Datadog, Inc.
 */

// Mock IAppHost* COM objects and lifecycle helpers shared by the tests that
// drive the module end to end (test_injection_pipeline.cpp,
// test_stable_config.cpp).

#pragma once

#include "config_reader.h"
#include "entrypoint.h"
#include "http_module.h"
#include "mocks.h"
#include "module_context.h"
#include "gtest/gtest.h"

#include <map>
#include <optional>
#include <string>
#include <utility>
#include <vector>

using ::testing::_;  // NOLINT(bugprone-reserved-identifier) — gtest matcher convention
using ::testing::DoAll;
using ::testing::Invoke;
using ::testing::Matcher;
using ::testing::Return;
using ::testing::SaveArg;
using ::testing::StrEq;

// ---------------------------------------------------------------------------
// Minimal IAppHost* COM mocks
// ---------------------------------------------------------------------------

class MockAppHostProperty : public IAppHostProperty {
  BSTR name_;
  VARIANT value_;
  bool default_value_ = false;

 public:
  MockAppHostProperty(LPCWSTR name, VARIANT val) : value_(val) { name_ = SysAllocString(name); }
  ~MockAppHostProperty() {
    SysFreeString(name_);
    VariantClear(&value_);
  }
  MockAppHostProperty(const MockAppHostProperty&) = delete;
  MockAppHostProperty& operator=(const MockAppHostProperty&) = delete;
  MockAppHostProperty(MockAppHostProperty&& other) noexcept
      : name_(other.name_), value_(other.value_) {
    other.name_ = nullptr;
    VariantInit(&other.value_);
  }
  MockAppHostProperty& operator=(MockAppHostProperty&&) = delete;

  ULONG STDMETHODCALLTYPE AddRef() override { return 1; }
  ULONG STDMETHODCALLTYPE Release() override { return 1; }
  HRESULT STDMETHODCALLTYPE QueryInterface(REFIID, void**) override { return E_NOINTERFACE; }

  HRESULT STDMETHODCALLTYPE get_Name(BSTR* pName) override {
    *pName = SysAllocString(name_);
    return S_OK;
  }
  HRESULT STDMETHODCALLTYPE get_Value(VARIANT* pValue) override {
    return VariantCopy(pValue, &value_);
  }
  HRESULT STDMETHODCALLTYPE put_Value(VARIANT) override { return E_NOTIMPL; }
  HRESULT STDMETHODCALLTYPE get_Schema(IAppHostPropertySchema**) override { return E_NOTIMPL; }
  void SetDefaultValue(bool value) { default_value_ = value; }
  HRESULT STDMETHODCALLTYPE GetMetadata(BSTR name, VARIANT* value) override {
    if (_wcsicmp(name, L"isDefaultValue") != 0) return E_NOTIMPL;
    VariantInit(value);
    value->vt = VT_BOOL;
    value->boolVal = default_value_ ? VARIANT_TRUE : VARIANT_FALSE;
    return S_OK;
  }
  HRESULT STDMETHODCALLTYPE SetMetadata(BSTR, VARIANT) override { return E_NOTIMPL; }
  HRESULT STDMETHODCALLTYPE get_Exception(IAppHostPropertyException**) override {
    return E_NOTIMPL;
  }
  HRESULT STDMETHODCALLTYPE Clear() override { return E_NOTIMPL; }
  HRESULT STDMETHODCALLTYPE get_StringValue(BSTR*) override { return E_NOTIMPL; }
};

static VARIANT bstr_variant(LPCWSTR s) {
  VARIANT v;
  VariantInit(&v);
  v.vt = VT_BSTR;
  v.bstrVal = SysAllocString(s);
  return v;
}

static VARIANT bool_variant(bool b) {
  VARIANT v;
  VariantInit(&v);
  v.vt = VT_BOOL;
  v.boolVal = b ? VARIANT_TRUE : VARIANT_FALSE;
  return v;
}

// An attribute that resolved to no value at all — what IIS hands back for an
// attribute with neither an explicit value nor a schema default.
static VARIANT empty_variant() {
  VARIANT v;
  VariantInit(&v);
  return v;
}

static VARIANT int_variant(int i) {
  VARIANT v;
  VariantInit(&v);
  v.vt = VT_I4;
  v.lVal = i;
  return v;
}

// A config option element with name/value properties.
class MockOptionElement : public IAppHostElement {
  MockAppHostProperty name_prop_;
  MockAppHostProperty value_prop_;

 public:
  MockOptionElement(LPCWSTR name, LPCWSTR value)
      : name_prop_(L"name", bstr_variant(name)), value_prop_(L"value", bstr_variant(value)) {}

  ULONG STDMETHODCALLTYPE AddRef() override { return 1; }
  ULONG STDMETHODCALLTYPE Release() override { return 1; }
  HRESULT STDMETHODCALLTYPE QueryInterface(REFIID, void**) override { return E_NOINTERFACE; }
  HRESULT STDMETHODCALLTYPE get_Name(BSTR*) override { return E_NOTIMPL; }
  HRESULT STDMETHODCALLTYPE get_Collection(IAppHostElementCollection**) override {
    return E_NOTIMPL;
  }
  HRESULT STDMETHODCALLTYPE get_ChildElements(IAppHostChildElementCollection**) override {
    return E_NOTIMPL;
  }
  HRESULT STDMETHODCALLTYPE GetMetadata(BSTR, VARIANT*) override { return E_NOTIMPL; }
  HRESULT STDMETHODCALLTYPE SetMetadata(BSTR, VARIANT) override { return E_NOTIMPL; }
  HRESULT STDMETHODCALLTYPE get_Schema(IAppHostElementSchema**) override { return E_NOTIMPL; }
  HRESULT STDMETHODCALLTYPE GetElementByName(BSTR, IAppHostElement**) override { return E_NOTIMPL; }
  HRESULT STDMETHODCALLTYPE GetPropertyByName(BSTR bstrName,
                                              IAppHostProperty** ppProperty) override {
    if (_wcsicmp(bstrName, L"name") == 0) {
      *ppProperty = &name_prop_;
      return S_OK;
    }
    if (_wcsicmp(bstrName, L"value") == 0) {
      *ppProperty = &value_prop_;
      return S_OK;
    }
    *ppProperty = nullptr;
    return S_FALSE;
  }
  HRESULT STDMETHODCALLTYPE get_Properties(IAppHostPropertyCollection**) override {
    return E_NOTIMPL;
  }
  HRESULT STDMETHODCALLTYPE Clear() override { return E_NOTIMPL; }
  HRESULT STDMETHODCALLTYPE get_Methods(IAppHostMethodCollection**) override { return E_NOTIMPL; }
};

class MockElementCollection : public IAppHostElementCollection {
  std::vector<MockOptionElement>& elements_;

 public:
  MockElementCollection(std::vector<MockOptionElement>& elems) : elements_(elems) {}

  ULONG STDMETHODCALLTYPE AddRef() override { return 1; }
  ULONG STDMETHODCALLTYPE Release() override { return 1; }
  HRESULT STDMETHODCALLTYPE QueryInterface(REFIID, void**) override { return E_NOINTERFACE; }
  HRESULT STDMETHODCALLTYPE get_Count(DWORD* pCount) override {
    *pCount = static_cast<DWORD>(elements_.size());
    return S_OK;
  }
  HRESULT STDMETHODCALLTYPE get_Item(VARIANT index, IAppHostElement** ppElement) override {
    const DWORD i = (index.vt == VT_UI4) ? index.ulVal : static_cast<DWORD>(index.iVal);
    if (i < elements_.size()) {
      *ppElement = &elements_[i];
      return S_OK;
    }
    *ppElement = nullptr;
    return E_FAIL;
  }
  HRESULT STDMETHODCALLTYPE AddElement(IAppHostElement*, INT) override { return E_NOTIMPL; }
  HRESULT STDMETHODCALLTYPE DeleteElement(VARIANT) override { return E_NOTIMPL; }
  HRESULT STDMETHODCALLTYPE Clear() override { return E_NOTIMPL; }
  HRESULT STDMETHODCALLTYPE CreateNewElement(BSTR, IAppHostElement**) override { return E_NOTIMPL; }
  HRESULT STDMETHODCALLTYPE get_Schema(IAppHostCollectionSchema**) override { return E_NOTIMPL; }
};

// Root datadogRum config element — configurable enabled, version, options.
class MockRumConfigElement : public IAppHostElement {
  MockAppHostProperty enabled_prop_;
  MockAppHostProperty version_prop_;
  // Unset by default, so the case every other test exercises really does go through the
  // "attribute missing" path.
  std::optional<MockAppHostProperty> injection_strategy_prop_;
  std::vector<MockOptionElement> options_;
  MockElementCollection collection_;
  HRESULT collection_result_ = S_OK;
  bool enabled_lookup_fails_ = false;

 public:
  // Configurable constructor. `version` is std::nullopt to model a section whose
  // `version` attribute resolved to nothing.
  MockRumConfigElement(bool enabled, std::vector<std::pair<LPCWSTR, LPCWSTR>> opts,
                       std::optional<int> version = 5)
      : enabled_prop_(L"enabled", bool_variant(enabled)),
        version_prop_(L"version", version.has_value() ? int_variant(*version) : empty_variant()),
        collection_(options_) {
    options_.reserve(opts.size());
    for (auto& [k, v] : opts) {
      options_.emplace_back(k, v);
    }
  }

  // Convenience: default test config.
  MockRumConfigElement()
      : MockRumConfigElement(true, {{L"applicationId", L"test-app-id"},
                                    {L"clientToken", L"test-client-token"},
                                    {L"site", L"datadoghq.com"},
                                    {L"service", L"test-service"},
                                    {L"sessionSampleRate", L"100"}}) {}

  ULONG STDMETHODCALLTYPE AddRef() override { return 1; }
  ULONG STDMETHODCALLTYPE Release() override { return 1; }
  HRESULT STDMETHODCALLTYPE QueryInterface(REFIID, void**) override { return E_NOINTERFACE; }
  void SetCollectionResult(HRESULT hr) { collection_result_ = hr; }
  void SetEnabledPropertyMissing(bool missing) { enabled_lookup_fails_ = missing; }
  void SetVersionDefault(bool value) { version_prop_.SetDefaultValue(value); }
  void SetInjectionStrategy(InjectionStrategy strategy) {
    injection_strategy_prop_.emplace(L"injectionStrategy", int_variant(static_cast<int>(strategy)));
  }

  HRESULT STDMETHODCALLTYPE get_Name(BSTR*) override { return E_NOTIMPL; }
  HRESULT STDMETHODCALLTYPE get_Collection(IAppHostElementCollection** ppCol) override {
    if (FAILED(collection_result_)) {
      *ppCol = nullptr;
      return collection_result_;
    }
    *ppCol = &collection_;
    return S_OK;
  }
  HRESULT STDMETHODCALLTYPE get_ChildElements(IAppHostChildElementCollection**) override {
    return E_NOTIMPL;
  }
  HRESULT STDMETHODCALLTYPE GetMetadata(BSTR, VARIANT*) override { return E_NOTIMPL; }
  HRESULT STDMETHODCALLTYPE SetMetadata(BSTR, VARIANT) override { return E_NOTIMPL; }
  HRESULT STDMETHODCALLTYPE get_Schema(IAppHostElementSchema**) override { return E_NOTIMPL; }
  HRESULT STDMETHODCALLTYPE GetElementByName(BSTR, IAppHostElement**) override { return E_NOTIMPL; }
  HRESULT STDMETHODCALLTYPE GetPropertyByName(BSTR bstrName,
                                              IAppHostProperty** ppProperty) override {
    if (_wcsicmp(bstrName, L"enabled") == 0) {
      if (enabled_lookup_fails_) {
        *ppProperty = nullptr;
        return S_FALSE;
      }
      *ppProperty = &enabled_prop_;
      return S_OK;
    }
    if (_wcsicmp(bstrName, L"version") == 0) {
      *ppProperty = &version_prop_;
      return S_OK;
    }
    if (injection_strategy_prop_ && _wcsicmp(bstrName, L"injectionStrategy") == 0) {
      *ppProperty = &*injection_strategy_prop_;
      return S_OK;
    }
    *ppProperty = nullptr;
    return S_FALSE;
  }
  HRESULT STDMETHODCALLTYPE get_Properties(IAppHostPropertyCollection**) override {
    return E_NOTIMPL;
  }
  HRESULT STDMETHODCALLTYPE Clear() override { return E_NOTIMPL; }
  HRESULT STDMETHODCALLTYPE get_Methods(IAppHostMethodCollection**) override { return E_NOTIMPL; }
};

// Path-aware admin manager: returns different config based on the config path
// argument to GetAdminSection (second parameter). This simulates how IIS
// resolves <location path="..."> overrides.
class MockAdminManager : public IAppHostAdminManager {
  // Map from config path → config element. Empty key = default for any path.
  std::map<std::wstring, MockRumConfigElement*> configs_;

 public:
  // Single config for all paths (original behavior).
  MockAdminManager(MockRumConfigElement* element) { configs_[L""] = element; }

  // Per-path configs.
  MockAdminManager(std::map<std::wstring, MockRumConfigElement*> configs)
      : configs_(std::move(configs)) {}

  HRESULT section_result_ = S_OK;
  void SetSectionResult(HRESULT result) { section_result_ = result; }

  void SetConfig(MockRumConfigElement* element) { configs_[L""] = element; }

  ULONG STDMETHODCALLTYPE AddRef() override { return 1; }
  ULONG STDMETHODCALLTYPE Release() override { return 1; }
  HRESULT STDMETHODCALLTYPE QueryInterface(REFIID, void**) override { return E_NOINTERFACE; }

  HRESULT STDMETHODCALLTYPE
  GetAdminSection(BSTR bstrSectionName,  // NOLINT(bugprone-easily-swappable-parameters)
                  BSTR bstrPath, IAppHostElement** ppElement) override {
    if (_wcsicmp(bstrSectionName, L"system.webServer/datadogRum") == 0) {
      if (FAILED(section_result_)) {
        *ppElement = nullptr;
        return section_result_;
      }
      // Look up by exact path first, then fall back to default.
      const std::wstring path(bstrPath ? bstrPath : L"");
      auto it = configs_.find(path);
      if (it != configs_.end()) {
        *ppElement = it->second;
        return S_OK;
      }
      // Try default (empty key).
      it = configs_.find(L"");
      if (it != configs_.end()) {
        *ppElement = it->second;
        return S_OK;
      }
      *ppElement = nullptr;
      return HRESULT_FROM_WIN32(ERROR_FILE_NOT_FOUND);
    }
    *ppElement = nullptr;
    return E_FAIL;
  }
  HRESULT STDMETHODCALLTYPE GetMetadata(BSTR, VARIANT*) override { return E_NOTIMPL; }
  HRESULT STDMETHODCALLTYPE SetMetadata(BSTR, VARIANT) override { return E_NOTIMPL; }
  HRESULT STDMETHODCALLTYPE get_ConfigManager(IAppHostConfigManager**) override {
    return E_NOTIMPL;
  }
};

// ---------------------------------------------------------------------------
// Mock module context container
// ---------------------------------------------------------------------------

class MockModuleContextContainer : public IHttpModuleContextContainer {
  IHttpStoredContext* ctx_ = nullptr;
  IHttpStoredContext* race_winner_ = nullptr;
  HRESULT set_result_ = S_OK;

 public:
  ~MockModuleContextContainer() {
    if (ctx_ != nullptr) {
      ctx_->CleanupStoredContext();
    }
  }

  void SetModuleContextResult(HRESULT hr) { set_result_ = hr; }

  // Simulates a race where another thread cached `winner` between the caller's
  // GetModuleContext (which returned null) and SetModuleContext: the SetModule
  // call rejects with ERROR_ALREADY_ASSIGNED, and a subsequent GetModule call
  // returns the winner.
  void SimulateRaceWith(IHttpStoredContext* winner) {
    race_winner_ = winner;
    set_result_ = HRESULT_FROM_WIN32(ERROR_ALREADY_ASSIGNED);
  }

  IHttpStoredContext* GetModuleContext(HTTP_MODULE_ID) override { return ctx_; }
  HRESULT SetModuleContext(IHttpStoredContext* ctx, HTTP_MODULE_ID) override {
    if (set_result_ == HRESULT_FROM_WIN32(ERROR_ALREADY_ASSIGNED)) {
      // Simulate race: pretend another thread inserted its context first so
      // the next GetModuleContext call sees the winner.
      if (race_winner_ != nullptr) {
        ctx_ = race_winner_;
        race_winner_ = nullptr;
      }
      return set_result_;
    }
    if (FAILED(set_result_)) {
      return set_result_;
    }
    ctx_ = ctx;
    return S_OK;
  }

  void PrePopulate(IHttpStoredContext* ctx) { ctx_ = ctx; }
};

// Mock IMetadataInfo — provides per-request config path and context container.
class MockMetadataInfo : public IMetadataInfo {
  std::wstring meta_path_;
  MockModuleContextContainer container_;
  bool return_null_container_ = false;

 public:
  MockMetadataInfo(LPCWSTR metaPath) : meta_path_(metaPath) {}

  MockModuleContextContainer& Container() { return container_; }
  void SetReturnNullContainer(bool v) { return_null_container_ = v; }

  PCWSTR GetMetaPath() const override { return meta_path_.c_str(); }
  PCWSTR GetVrPath() const override { return L""; }
  IHttpTokenEntry* GetVrToken() override { return nullptr; }
  IHttpModuleContextContainer* GetModuleContextContainer() override {
    return return_null_container_ ? nullptr : &container_;
  }
};

// MockHttpApplication with configurable config path.
class MockAppWithContext : public IHttpApplication {
  MockModuleContextContainer container_;
  std::wstring config_path_;

 public:
  MockAppWithContext(LPCWSTR configPath = L"MACHINE/WEBROOT/APPHOST/Default Web Site")
      : config_path_(configPath) {}

  PCWSTR GetApplicationPhysicalPath() const override { return L"C:\\test"; }
  PCWSTR GetApplicationId() const override { return L"/"; }
  PCWSTR GetAppConfigPath() const override { return config_path_.c_str(); }
  IHttpModuleContextContainer* GetModuleContextContainer() override { return &container_; }
};

class MockAppStartProvider : public IHttpApplicationStartProvider {
  IHttpApplication& app_;

 public:
  MockAppStartProvider(IHttpApplication& app) : app_(app) {}
  IHttpApplication* GetApplication() override { return &app_; }
  VOID SetErrorStatus(HRESULT) override {}
};

class MockConfigChangeProvider : public IGlobalConfigurationChangeProvider {
  std::wstring path_;

 public:
  MockConfigChangeProvider(LPCWSTR path) : path_(path) {}
  PCWSTR GetChangePath() const override { return path_.c_str(); }
  VOID SetErrorStatus(HRESULT) override {}
};

// ---------------------------------------------------------------------------
// Helpers to reduce test boilerplate
// ---------------------------------------------------------------------------

struct RegisteredModule {
  CGlobalModule* global;
  IHttpModuleFactory* factory;
};

// Register the module with the given admin manager. Returns the global module
// and HTTP factory for driving the rest of the lifecycle.
static RegisteredModule registerModule(MockServer& server) {
  // These tests assert on IIS-sourced configuration only. Point GlobalModule's
  // stable-config read at paths that cannot exist, so the result does not depend
  // on whether the host running the tests has an agent-written
  // application_monitoring.yaml. Without this, InjectionPipeline.MissingConfig
  // fails on any machine with a configured agent.
  // A plain absent path, not a malformed one: the read must come back "not
  // found" rather than as an error the module would log and report.
  datadog::rum::set_stable_config_paths_for_testing(
      "C:\\iis-injector-tests-no-such-dir\\no-local-stable-config.yaml",
      "C:\\iis-injector-tests-no-such-dir\\no-fleet-stable-config.yaml");

  MockModule moduleInfo;
  EXPECT_CALL(moduleInfo, GetId()).WillOnce(Return(nullptr));

  RegisteredModule result{};
  ON_CALL(moduleInfo, SetGlobalNotifications)
      .WillByDefault(DoAll(SaveArg<0>(&result.global), Return(S_OK)));
  EXPECT_CALL(moduleInfo, SetGlobalNotifications(_, _)).Times(1);

  ON_CALL(moduleInfo, SetRequestNotifications)
      .WillByDefault(DoAll(SaveArg<0>(&result.factory), Return(S_OK)));
  EXPECT_CALL(moduleInfo, SetRequestNotifications(_, _, _)).Times(1);

  const HRESULT hr = RegisterModule(0, &moduleInfo, &server);
  EXPECT_EQ(hr, S_OK);
  return result;
}

// Set up a mock HTTP response that looks like a 200 text/html with the given
// HTML body. Returns the raw HTTP_RESPONSE (caller owns the chunk memory).
struct PreparedResponse {
  HTTP_DATA_CHUNK chunk;
  HTTP_RESPONSE raw;
};

// The entity chunks of a response whose body is split across several of them, as IIS does for a
// streamed response. The bodies are owned here, so they outlive the chunks pointing into them.
struct PreparedChunkedResponse {
  std::vector<std::string> bodies;
  std::vector<HTTP_DATA_CHUNK> chunks;
  HTTP_RESPONSE raw;
};

// Everything but the entity chunks: a 200 text/html response, uncompressed and not yet injected.
static void setupHtmlResponseMetadata(MockHttpContext& ctx, MockHttpResponse& resp,
                                      IHttpApplication* app, MockMetadataInfo* metadata) {
  EXPECT_CALL(ctx, GetApplication()).WillRepeatedly(Return(app));
  EXPECT_CALL(ctx, GetResponse()).WillRepeatedly(Return(&resp));
  EXPECT_CALL(ctx, GetMetadata()).WillRepeatedly(Return(metadata));

  const char* ct = "text/html";
  EXPECT_CALL(resp, GetHeader(Matcher<PCSTR>(StrEq("Content-Type")), _))
      .WillRepeatedly(DoAll(
          Invoke([ct](_In_ PCSTR, _Out_ USHORT* len) { *len = (USHORT)strlen(ct); }), Return(ct)));
  EXPECT_CALL(resp, GetHeader(Matcher<PCSTR>(StrEq("Content-Encoding")), _))
      .WillRepeatedly(DoAll(Invoke([](_In_ PCSTR, _Out_ USHORT* len) { *len = 0; }), Return("")));
  EXPECT_CALL(resp, GetHeader(Matcher<PCSTR>(StrEq(datadog::rum::k_injection_header)), _))
      .WillRepeatedly(DoAll(Invoke([](_In_ PCSTR, _Out_ USHORT* len) { *len = 0; }), Return("")));

  EXPECT_CALL(resp, GetStatus(_, _, _, _, _, _, _, _, _))
      .WillRepeatedly(DoAll(Invoke([](_Out_ USHORT* pCode, _Out_ USHORT*, PCSTR*, _Out_ USHORT*,
                                      _Out_ HRESULT*, PCWSTR*, _Out_ DWORD*,
                                      IAppHostConfigException**, _Out_ BOOL*) { *pCode = 200; }),
                            Return()));
}

static void setupHtmlResponse(MockHttpContext& ctx, MockHttpResponse& resp, IHttpApplication* app,
                              MockMetadataInfo* metadata, PreparedResponse& pr, const char* html) {
  setupHtmlResponseMetadata(ctx, resp, app, metadata);

  pr.chunk.DataChunkType = HttpDataChunkFromMemory;
  pr.chunk.FromMemory.pBuffer = (PVOID)html;
  pr.chunk.FromMemory.BufferLength = (ULONG)strlen(html);
  pr.raw = {};
  pr.raw.EntityChunkCount = 1;
  pr.raw.pEntityChunks = &pr.chunk;

  EXPECT_CALL(resp, GetRawHttpResponse()).WillRepeatedly(Return(&pr.raw));
}
