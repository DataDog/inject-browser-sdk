// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2024-Present Datadog, Inc.

// Integration-style test that exercises the full IIS module pipeline:
//   RegisterModule → OnGlobalApplicationStart (config reading)
//     → OnBeginRequest (pending header) / OnSendResponse (injection into HTML)
//
// Uses mock IAppHost* COM interfaces to feed configuration, and a mock
// HTTP response containing an HTML body with <head></head>.

#include "entrypoint.h"
#include "http_module.h"
#include "module_context.h"
#include "mocks.h"
#include "gtest/gtest.h"

#include <map>
#include <utility>

using namespace datadog::rum;
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
  HRESULT STDMETHODCALLTYPE GetMetadata(BSTR, VARIANT*) override { return E_NOTIMPL; }
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
  std::vector<MockOptionElement> options_;
  MockElementCollection collection_;
  HRESULT collection_result_ = S_OK;
  bool enabled_lookup_fails_ = false;

 public:
  // Configurable constructor.
  MockRumConfigElement(bool enabled, std::vector<std::pair<LPCWSTR, LPCWSTR>> opts)
      : enabled_prop_(L"enabled", bool_variant(enabled)),
        version_prop_(L"version", int_variant(5)),
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

  // Swap the default config (used by reload tests).
  void SetConfig(MockRumConfigElement* element) { configs_[L""] = element; }

  ULONG STDMETHODCALLTYPE AddRef() override { return 1; }
  ULONG STDMETHODCALLTYPE Release() override { return 1; }
  HRESULT STDMETHODCALLTYPE QueryInterface(REFIID, void**) override { return E_NOINTERFACE; }

  HRESULT STDMETHODCALLTYPE
  GetAdminSection(BSTR bstrSectionName,  // NOLINT(bugprone-easily-swappable-parameters)
                  BSTR bstrPath, IAppHostElement** ppElement) override {
    if (_wcsicmp(bstrSectionName, L"system.webServer/datadogRum") == 0) {
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
      return E_FAIL;
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

static void setupHtmlResponse(MockHttpContext& ctx, MockHttpResponse& resp, IHttpApplication* app,
                              MockMetadataInfo* metadata, PreparedResponse& pr, const char* html) {
  EXPECT_CALL(ctx, GetApplication()).WillRepeatedly(Return(app));
  EXPECT_CALL(ctx, GetResponse()).WillRepeatedly(Return(&resp));
  EXPECT_CALL(ctx, GetMetadata()).WillRepeatedly(Return(metadata));

  const char* ct = "text/html";
  EXPECT_CALL(resp, GetHeader(Matcher<PCSTR>(StrEq("Content-Type")), _))
      .WillRepeatedly(DoAll(
          Invoke([ct](_In_ PCSTR, _Out_ USHORT* len) { *len = (USHORT)strlen(ct); }), Return(ct)));
  EXPECT_CALL(resp, GetHeader(Matcher<PCSTR>(StrEq("Content-Encoding")), _))
      .WillRepeatedly(DoAll(Invoke([](_In_ PCSTR, _Out_ USHORT* len) { *len = 0; }), Return("")));
  EXPECT_CALL(resp, GetHeader(Matcher<PCSTR>(StrEq(k_injection_header)), _))
      .WillRepeatedly(DoAll(Invoke([](_In_ PCSTR, _Out_ USHORT* len) { *len = 0; }), Return("")));

  EXPECT_CALL(resp, GetStatus(_, _, _, _, _, _, _, _, _))
      .WillRepeatedly(DoAll(Invoke([](_Out_ USHORT* pCode, _Out_ USHORT*, PCSTR*, _Out_ USHORT*,
                                      _Out_ HRESULT*, PCWSTR*, _Out_ DWORD*,
                                      IAppHostConfigException**, _Out_ BOOL*) { *pCode = 200; }),
                            Return()));

  pr.chunk.DataChunkType = HttpDataChunkFromMemory;
  pr.chunk.FromMemory.pBuffer = (PVOID)html;
  pr.chunk.FromMemory.BufferLength = (ULONG)strlen(html);
  pr.raw = {};
  pr.raw.EntityChunkCount = 1;
  pr.raw.pEntityChunks = &pr.chunk;

  EXPECT_CALL(resp, GetRawHttpResponse()).WillRepeatedly(Return(&pr.raw));
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

TEST(InjectionPipeline, FullModuleLifecycle) {
  MockRumConfigElement rumConfig;
  MockAdminManager adminManager(&rumConfig);
  MockServer mockServer;
  EXPECT_CALL(mockServer, GetAdminManager()).WillRepeatedly(Return(&adminManager));

  auto mod = registerModule(mockServer);
  ASSERT_NE(mod.global, nullptr);
  ASSERT_NE(mod.factory, nullptr);

  // App start → config reading
  MockAppWithContext app;
  MockAppStartProvider appStart(app);
  EXPECT_EQ(mod.global->OnGlobalApplicationStart(&appStart), GL_NOTIFICATION_CONTINUE);

  auto* ctx =
      static_cast<ModuleContext*>(app.GetModuleContextContainer()->GetModuleContext(nullptr));
  ASSERT_NE(ctx, nullptr);
  ASSERT_NE(ctx->snippet, nullptr);

  // HTTP module
  CHttpModule* httpMod = nullptr;
  ASSERT_EQ(mod.factory->GetHttpModule(&httpMod, nullptr), S_OK);

  // Simulate response
  MockHttpContext httpCtx;
  MockHttpResponse httpResp;
  MockMetadataInfo metadata(L"MACHINE/WEBROOT/APPHOST/Default Web Site");
  PreparedResponse pr;
  const char* html =
      "<!DOCTYPE html><html><head><title>Test</title></head>"
      "<body>Hello</body></html>";
  setupHtmlResponse(httpCtx, httpResp, &app, &metadata, pr, html);

  EXPECT_CALL(httpResp, SetHeader(Matcher<PCSTR>(StrEq(k_injection_header)), _, _, _))
      .WillOnce(Return(S_OK));
  EXPECT_CALL(httpResp, Flush(_, _, _, _)).WillOnce(Return(S_OK));

  EXPECT_EQ(httpMod->OnSendResponse(&httpCtx, nullptr), RQ_NOTIFICATION_FINISH_REQUEST);

  const std::string output(static_cast<const char*>(pr.chunk.FromMemory.pBuffer),
                           pr.chunk.FromMemory.BufferLength);
  EXPECT_NE(output.find("DD_RUM"), std::string::npos) << "Expected DD_RUM in output, got:\n"
                                                      << output;
  EXPECT_NE(output.find("test-app-id"), std::string::npos);
  EXPECT_NE(output.find("test-client-token"), std::string::npos);
  EXPECT_NE(output.find("test-service"), std::string::npos);
  EXPECT_NE(output.find("</head>"), std::string::npos);

  mod.factory->Terminate();
  mod.global->Terminate();
}

TEST(InjectionPipeline, DisabledConfig) {
  MockRumConfigElement rumConfig(false, {
                                            {L"applicationId", L"test-app-id"},
                                            {L"clientToken", L"test-client-token"},
                                        });
  MockAdminManager adminManager(&rumConfig);
  MockServer mockServer;
  EXPECT_CALL(mockServer, GetAdminManager()).WillRepeatedly(Return(&adminManager));

  auto mod = registerModule(mockServer);

  MockAppWithContext app;
  MockAppStartProvider appStart(app);
  mod.global->OnGlobalApplicationStart(&appStart);

  auto* ctx =
      static_cast<ModuleContext*>(app.GetModuleContextContainer()->GetModuleContext(nullptr));
  ASSERT_NE(ctx, nullptr);
  // When disabled, no snippet should be created.
  EXPECT_EQ(ctx->snippet, nullptr);

  CHttpModule* httpMod = nullptr;
  mod.factory->GetHttpModule(&httpMod, nullptr);

  MockHttpContext httpCtx;
  MockHttpResponse httpResp;
  MockMetadataInfo metadata(L"MACHINE/WEBROOT/APPHOST/Default Web Site");
  PreparedResponse pr;
  const char* html = "<html><head></head><body>Hello</body></html>";
  setupHtmlResponse(httpCtx, httpResp, &app, &metadata, pr, html);

  // OnSendResponse should skip injection (CONTINUE, not FINISH_REQUEST).
  EXPECT_EQ(httpMod->OnSendResponse(&httpCtx, nullptr), RQ_NOTIFICATION_CONTINUE);

  // Body unchanged.
  const std::string output(static_cast<const char*>(pr.chunk.FromMemory.pBuffer),
                           pr.chunk.FromMemory.BufferLength);
  EXPECT_EQ(output, html);

  mod.factory->Terminate();
  mod.global->Terminate();
}

// Per-location config: two different metadata paths within the same application
// get different config. The admin manager returns different config depending on
// the config path, and IMetadataInfo::GetMetaPath() provides the resolved path.
TEST(InjectionPipeline, PerLocationConfig) {
  // Path A: RUM enabled with service "service-a"
  MockRumConfigElement configA(true, {
                                         {L"applicationId", L"test-app-id"},
                                         {L"clientToken", L"test-client-token"},
                                         {L"service", L"service-a"},
                                         {L"sessionSampleRate", L"100"},
                                     });
  // Path B: RUM disabled
  MockRumConfigElement configB(false, {});

  std::map<std::wstring, MockRumConfigElement*> configs;
  configs[L"MACHINE/WEBROOT/APPHOST/Default Web Site"] = &configA;
  configs[L"MACHINE/WEBROOT/APPHOST/Default Web Site/api"] = &configB;
  MockAdminManager adminManager(configs);

  MockServer mockServer;
  EXPECT_CALL(mockServer, GetAdminManager()).WillRepeatedly(Return(&adminManager));

  auto mod = registerModule(mockServer);

  // Single application — both paths are within the same app.
  MockAppWithContext app(L"MACHINE/WEBROOT/APPHOST/Default Web Site");
  MockAppStartProvider appStart(app);
  mod.global->OnGlobalApplicationStart(&appStart);

  CHttpModule* httpMod = nullptr;
  mod.factory->GetHttpModule(&httpMod, nullptr);

  const char* html =
      "<html><head><title>Test</title></head>"
      "<body>Hello</body></html>";

  // Request 1: metadata path = site root → should inject with service-a
  {
    MockHttpContext httpCtx;
    MockHttpResponse httpResp;
    MockMetadataInfo metadata(L"MACHINE/WEBROOT/APPHOST/Default Web Site");
    PreparedResponse pr;
    setupHtmlResponse(httpCtx, httpResp, &app, &metadata, pr, html);
    EXPECT_CALL(httpResp, SetHeader(Matcher<PCSTR>(StrEq(k_injection_header)), _, _, _))
        .WillOnce(Return(S_OK));
    EXPECT_CALL(httpResp, Flush(_, _, _, _)).WillOnce(Return(S_OK));

    EXPECT_EQ(httpMod->OnSendResponse(&httpCtx, nullptr), RQ_NOTIFICATION_FINISH_REQUEST);

    const std::string output(static_cast<const char*>(pr.chunk.FromMemory.pBuffer),
                             pr.chunk.FromMemory.BufferLength);
    EXPECT_NE(output.find("service-a"), std::string::npos)
        << "Root path should inject with service-a, got:\n"
        << output;
  }

  // Request 2: metadata path = /api → should NOT inject (location override)
  {
    MockHttpContext httpCtx;
    MockHttpResponse httpResp;
    MockMetadataInfo metadata(L"MACHINE/WEBROOT/APPHOST/Default Web Site/api");
    PreparedResponse pr;
    setupHtmlResponse(httpCtx, httpResp, &app, &metadata, pr, html);

    EXPECT_EQ(httpMod->OnSendResponse(&httpCtx, nullptr), RQ_NOTIFICATION_CONTINUE)
        << "/api path should skip injection (location override disabled RUM)";

    const std::string output(static_cast<const char*>(pr.chunk.FromMemory.pBuffer),
                             pr.chunk.FromMemory.BufferLength);
    EXPECT_EQ(output, html) << "/api path response should be unchanged";
  }

  mod.factory->Terminate();
  mod.global->Terminate();
}

// ---------------------------------------------------------------------------
// Edge-case and error-path tests
// ---------------------------------------------------------------------------

// Cache hit: second request to the same metadata path uses cached context.
TEST(InjectionPipeline, MetadataCacheHit) {
  MockRumConfigElement rumConfig;
  MockAdminManager adminManager(&rumConfig);
  MockServer mockServer;
  EXPECT_CALL(mockServer, GetAdminManager()).WillRepeatedly(Return(&adminManager));

  auto mod = registerModule(mockServer);
  MockAppWithContext app;
  MockAppStartProvider appStart(app);
  mod.global->OnGlobalApplicationStart(&appStart);

  CHttpModule* httpMod = nullptr;
  mod.factory->GetHttpModule(&httpMod, nullptr);

  const char* html = "<html><head></head><body>Hello</body></html>";
  MockMetadataInfo metadata(L"MACHINE/WEBROOT/APPHOST/Default Web Site");

  // First request — cache miss, populates metadata container.
  {
    MockHttpContext httpCtx;
    MockHttpResponse httpResp;
    PreparedResponse pr;
    setupHtmlResponse(httpCtx, httpResp, &app, &metadata, pr, html);
    EXPECT_CALL(httpResp, SetHeader(Matcher<PCSTR>(StrEq(k_injection_header)), _, _, _))
        .WillOnce(Return(S_OK));
    EXPECT_CALL(httpResp, Flush(_, _, _, _)).WillOnce(Return(S_OK));
    EXPECT_EQ(httpMod->OnSendResponse(&httpCtx, nullptr), RQ_NOTIFICATION_FINISH_REQUEST);
  }

  // Second request — cache hit, same metadata object.
  {
    MockHttpContext httpCtx;
    MockHttpResponse httpResp;
    PreparedResponse pr;
    setupHtmlResponse(httpCtx, httpResp, &app, &metadata, pr, html);
    EXPECT_CALL(httpResp, SetHeader(Matcher<PCSTR>(StrEq(k_injection_header)), _, _, _))
        .WillOnce(Return(S_OK));
    EXPECT_CALL(httpResp, Flush(_, _, _, _)).WillOnce(Return(S_OK));
    EXPECT_EQ(httpMod->OnSendResponse(&httpCtx, nullptr), RQ_NOTIFICATION_FINISH_REQUEST);

    const std::string output(static_cast<const char*>(pr.chunk.FromMemory.pBuffer),
                             pr.chunk.FromMemory.BufferLength);
    EXPECT_NE(output.find("DD_RUM"), std::string::npos);
  }

  mod.factory->Terminate();
  mod.global->Terminate();
}

// SetModuleContext returns ERROR_ALREADY_ASSIGNED (race condition).
TEST(InjectionPipeline, SetModuleContextRace) {
  MockRumConfigElement rumConfig;
  MockAdminManager adminManager(&rumConfig);
  MockServer mockServer;
  EXPECT_CALL(mockServer, GetAdminManager()).WillRepeatedly(Return(&adminManager));

  auto mod = registerModule(mockServer);
  MockAppWithContext app;
  MockAppStartProvider appStart(app);
  mod.global->OnGlobalApplicationStart(&appStart);

  CHttpModule* httpMod = nullptr;
  mod.factory->GetHttpModule(&httpMod, nullptr);

  // Simulate the race the production code is trying to handle: this thread's
  // GetModuleContext returns null (cache miss), so it builds new_ctx and
  // calls SetModuleContext, which is rejected with ERROR_ALREADY_ASSIGNED
  // because another thread inserted winner_ctx first. The second
  // GetModuleContext then returns winner_ctx.
  MockMetadataInfo metadata(L"MACHINE/WEBROOT/APPHOST/Default Web Site");
  auto* winner_ctx = new ModuleContext;
  auto* app_ctx =
      static_cast<ModuleContext*>(app.GetModuleContextContainer()->GetModuleContext(nullptr));
  winner_ctx->logger = app_ctx->logger;
  winner_ctx->snippet = snippet_create_from_json(
      R"({"majorVersion":5,"rum":{"applicationId":"test-app-id","clientToken":"test-client-token","service":"test-service","sessionSampleRate":100}})");
  ASSERT_NE(winner_ctx->snippet, nullptr);
  ASSERT_EQ(winner_ctx->snippet->error_code, 0u);
  metadata.Container().SimulateRaceWith(winner_ctx);

  const char* html = "<html><head></head><body>Hello</body></html>";
  MockHttpContext httpCtx;
  MockHttpResponse httpResp;
  PreparedResponse pr;
  setupHtmlResponse(httpCtx, httpResp, &app, &metadata, pr, html);
  EXPECT_CALL(httpResp, SetHeader(Matcher<PCSTR>(StrEq(k_injection_header)), _, _, _))
      .WillOnce(Return(S_OK));
  EXPECT_CALL(httpResp, Flush(_, _, _, _)).WillOnce(Return(S_OK));

  EXPECT_EQ(httpMod->OnSendResponse(&httpCtx, nullptr), RQ_NOTIFICATION_FINISH_REQUEST);

  // winner_ctx is owned by metadata.Container() and freed by its destructor.

  mod.factory->Terminate();
  mod.global->Terminate();
}

// SetModuleContext returns a generic failure — no injection, no crash.
TEST(InjectionPipeline, SetModuleContextFailure) {
  MockRumConfigElement rumConfig;
  MockAdminManager adminManager(&rumConfig);
  MockServer mockServer;
  EXPECT_CALL(mockServer, GetAdminManager()).WillRepeatedly(Return(&adminManager));

  auto mod = registerModule(mockServer);
  MockAppWithContext app;
  MockAppStartProvider appStart(app);
  mod.global->OnGlobalApplicationStart(&appStart);

  CHttpModule* httpMod = nullptr;
  mod.factory->GetHttpModule(&httpMod, nullptr);

  MockMetadataInfo metadata(L"MACHINE/WEBROOT/APPHOST/Default Web Site");
  metadata.Container().SetModuleContextResult(E_FAIL);

  const char* html = "<html><head></head><body>Hello</body></html>";
  MockHttpContext httpCtx;
  MockHttpResponse httpResp;
  PreparedResponse pr;
  setupHtmlResponse(httpCtx, httpResp, &app, &metadata, pr, html);

  EXPECT_EQ(httpMod->OnSendResponse(&httpCtx, nullptr), RQ_NOTIFICATION_CONTINUE);

  const std::string output(static_cast<const char*>(pr.chunk.FromMemory.pBuffer),
                           pr.chunk.FromMemory.BufferLength);
  EXPECT_EQ(output, html);

  mod.factory->Terminate();
  mod.global->Terminate();
}

// Null metadata — falls back to app-level context and injects.
TEST(InjectionPipeline, NullMetadataFallback) {
  MockRumConfigElement rumConfig;
  MockAdminManager adminManager(&rumConfig);
  MockServer mockServer;
  EXPECT_CALL(mockServer, GetAdminManager()).WillRepeatedly(Return(&adminManager));

  auto mod = registerModule(mockServer);
  MockAppWithContext app;
  MockAppStartProvider appStart(app);
  mod.global->OnGlobalApplicationStart(&appStart);

  CHttpModule* httpMod = nullptr;
  mod.factory->GetHttpModule(&httpMod, nullptr);

  const char* html = "<html><head></head><body>Hello</body></html>";
  MockHttpContext httpCtx;
  MockHttpResponse httpResp;
  PreparedResponse pr;
  setupHtmlResponse(httpCtx, httpResp, &app, nullptr, pr, html);
  EXPECT_CALL(httpResp, SetHeader(Matcher<PCSTR>(StrEq(k_injection_header)), _, _, _))
      .WillOnce(Return(S_OK));
  EXPECT_CALL(httpResp, Flush(_, _, _, _)).WillOnce(Return(S_OK));

  EXPECT_EQ(httpMod->OnSendResponse(&httpCtx, nullptr), RQ_NOTIFICATION_FINISH_REQUEST);

  mod.factory->Terminate();
  mod.global->Terminate();
}

// Null metadata context container — falls back to app-level context and
// injects.
TEST(InjectionPipeline, NullMetadataContainerFallback) {
  MockRumConfigElement rumConfig;
  MockAdminManager adminManager(&rumConfig);
  MockServer mockServer;
  EXPECT_CALL(mockServer, GetAdminManager()).WillRepeatedly(Return(&adminManager));

  auto mod = registerModule(mockServer);
  MockAppWithContext app;
  MockAppStartProvider appStart(app);
  mod.global->OnGlobalApplicationStart(&appStart);

  CHttpModule* httpMod = nullptr;
  mod.factory->GetHttpModule(&httpMod, nullptr);

  MockMetadataInfo metadata(L"MACHINE/WEBROOT/APPHOST/Default Web Site");
  metadata.SetReturnNullContainer(true);

  const char* html = "<html><head></head><body>Hello</body></html>";
  MockHttpContext httpCtx;
  MockHttpResponse httpResp;
  PreparedResponse pr;
  setupHtmlResponse(httpCtx, httpResp, &app, &metadata, pr, html);
  EXPECT_CALL(httpResp, SetHeader(Matcher<PCSTR>(StrEq(k_injection_header)), _, _, _))
      .WillOnce(Return(S_OK));
  EXPECT_CALL(httpResp, Flush(_, _, _, _)).WillOnce(Return(S_OK));

  // Falls back to app-level context and injects.
  EXPECT_EQ(httpMod->OnSendResponse(&httpCtx, nullptr), RQ_NOTIFICATION_FINISH_REQUEST);

  mod.factory->Terminate();
  mod.global->Terminate();
}

// CleanupStoredContext properly frees snippet.
TEST(InjectionPipeline, CleanupStoredContextFreesSnippet) {
  auto* ctx = new ModuleContext;
  ctx->snippet = snippet_create_from_json(
      R"({"majorVersion":5,"rum":{"applicationId":"test","clientToken":"tok"}})");
  ASSERT_NE(ctx->snippet, nullptr);

  // Under ASan this catches use-after-free or double-free.
  ctx->CleanupStoredContext();
}

// Two enabled locations with different services — verify per-path isolation.
TEST(InjectionPipeline, TwoEnabledLocationsDistinctConfig) {
  MockRumConfigElement configA(true, {
                                         {L"applicationId", L"app-a"},
                                         {L"clientToken", L"tok-a"},
                                         {L"service", L"svc-alpha"},
                                         {L"sessionSampleRate", L"100"},
                                     });
  MockRumConfigElement configB(true, {
                                         {L"applicationId", L"app-b"},
                                         {L"clientToken", L"tok-b"},
                                         {L"service", L"svc-beta"},
                                         {L"sessionSampleRate", L"100"},
                                     });

  std::map<std::wstring, MockRumConfigElement*> configs;
  configs[L"MACHINE/WEBROOT/APPHOST/Default Web Site"] = &configA;
  configs[L"MACHINE/WEBROOT/APPHOST/Default Web Site/blog"] = &configB;
  MockAdminManager adminManager(configs);

  MockServer mockServer;
  EXPECT_CALL(mockServer, GetAdminManager()).WillRepeatedly(Return(&adminManager));

  auto mod = registerModule(mockServer);
  MockAppWithContext app(L"MACHINE/WEBROOT/APPHOST/Default Web Site");
  MockAppStartProvider appStart(app);
  mod.global->OnGlobalApplicationStart(&appStart);

  CHttpModule* httpMod = nullptr;
  mod.factory->GetHttpModule(&httpMod, nullptr);

  const char* html = "<html><head></head><body>Hello</body></html>";

  // Root — svc-alpha
  {
    MockHttpContext httpCtx;
    MockHttpResponse httpResp;
    MockMetadataInfo metadata(L"MACHINE/WEBROOT/APPHOST/Default Web Site");
    PreparedResponse pr;
    setupHtmlResponse(httpCtx, httpResp, &app, &metadata, pr, html);
    EXPECT_CALL(httpResp, SetHeader(Matcher<PCSTR>(StrEq(k_injection_header)), _, _, _))
        .WillOnce(Return(S_OK));
    EXPECT_CALL(httpResp, Flush(_, _, _, _)).WillOnce(Return(S_OK));

    EXPECT_EQ(httpMod->OnSendResponse(&httpCtx, nullptr), RQ_NOTIFICATION_FINISH_REQUEST);
    const std::string output(static_cast<const char*>(pr.chunk.FromMemory.pBuffer),
                             pr.chunk.FromMemory.BufferLength);
    EXPECT_NE(output.find("svc-alpha"), std::string::npos);
    EXPECT_EQ(output.find("svc-beta"), std::string::npos);
  }

  // /blog — svc-beta
  {
    MockHttpContext httpCtx;
    MockHttpResponse httpResp;
    MockMetadataInfo metadata(L"MACHINE/WEBROOT/APPHOST/Default Web Site/blog");
    PreparedResponse pr;
    setupHtmlResponse(httpCtx, httpResp, &app, &metadata, pr, html);
    EXPECT_CALL(httpResp, SetHeader(Matcher<PCSTR>(StrEq(k_injection_header)), _, _, _))
        .WillOnce(Return(S_OK));
    EXPECT_CALL(httpResp, Flush(_, _, _, _)).WillOnce(Return(S_OK));

    EXPECT_EQ(httpMod->OnSendResponse(&httpCtx, nullptr), RQ_NOTIFICATION_FINISH_REQUEST);
    const std::string output(static_cast<const char*>(pr.chunk.FromMemory.pBuffer),
                             pr.chunk.FromMemory.BufferLength);
    EXPECT_NE(output.find("svc-beta"), std::string::npos);
    EXPECT_EQ(output.find("svc-alpha"), std::string::npos);
  }

  mod.factory->Terminate();
  mod.global->Terminate();
}

// OnBeginRequest uses per-location cache: pending header should NOT be set
// for a path where RUM is disabled via <location> override (after cache is
// populated by a prior OnSendResponse).
TEST(InjectionPipeline, OnBeginRequestRespectsLocationOverride) {
  MockRumConfigElement configA(true, {
                                         {L"applicationId", L"test-app-id"},
                                         {L"clientToken", L"test-client-token"},
                                         {L"service", L"service-a"},
                                         {L"sessionSampleRate", L"100"},
                                     });
  MockRumConfigElement configB(false, {});

  std::map<std::wstring, MockRumConfigElement*> configs;
  configs[L"MACHINE/WEBROOT/APPHOST/Default Web Site"] = &configA;
  configs[L"MACHINE/WEBROOT/APPHOST/Default Web Site/api"] = &configB;
  MockAdminManager adminManager(configs);

  MockServer mockServer;
  EXPECT_CALL(mockServer, GetAdminManager()).WillRepeatedly(Return(&adminManager));

  auto mod = registerModule(mockServer);
  MockAppWithContext app(L"MACHINE/WEBROOT/APPHOST/Default Web Site");
  MockAppStartProvider appStart(app);
  mod.global->OnGlobalApplicationStart(&appStart);

  CHttpModule* httpMod = nullptr;
  mod.factory->GetHttpModule(&httpMod, nullptr);

  const char* html = "<html><head></head><body>Hello</body></html>";

  // First: OnSendResponse to /api to populate the metadata cache (disabled).
  MockMetadataInfo apiMetadata(L"MACHINE/WEBROOT/APPHOST/Default Web Site/api");
  {
    MockHttpContext httpCtx;
    MockHttpResponse httpResp;
    PreparedResponse pr;
    setupHtmlResponse(httpCtx, httpResp, &app, &apiMetadata, pr, html);
    EXPECT_EQ(httpMod->OnSendResponse(&httpCtx, nullptr), RQ_NOTIFICATION_CONTINUE);
  }

  // Now: OnBeginRequest to /api — should NOT set the pending header because
  // the per-location cache says disabled.
  {
    MockHttpContext httpCtx;
    MockHttpRequest httpReq;
    MockEventProvider eventProvider;
    EXPECT_CALL(httpCtx, GetRequest()).WillRepeatedly(Return(&httpReq));
    EXPECT_CALL(httpCtx, GetApplication()).WillRepeatedly(Return(&app));
    EXPECT_CALL(httpCtx, GetMetadata()).WillRepeatedly(Return(&apiMetadata));

    // The pending header should NOT be set.
    EXPECT_CALL(httpReq, SetHeader(Matcher<PCSTR>(StrEq(k_injection_pending_header)), _, _, _))
        .Times(0);

    EXPECT_EQ(httpMod->OnBeginRequest(&httpCtx, &eventProvider), RQ_NOTIFICATION_CONTINUE);
  }

  // Contrast: OnBeginRequest to root — SHOULD set the pending header.
  MockMetadataInfo rootMetadata(L"MACHINE/WEBROOT/APPHOST/Default Web Site");
  {
    // First populate root cache via OnSendResponse.
    MockHttpContext httpCtx;
    MockHttpResponse httpResp;
    PreparedResponse pr;
    setupHtmlResponse(httpCtx, httpResp, &app, &rootMetadata, pr, html);
    EXPECT_CALL(httpResp, SetHeader(Matcher<PCSTR>(StrEq(k_injection_header)), _, _, _))
        .WillOnce(Return(S_OK));
    EXPECT_CALL(httpResp, Flush(_, _, _, _)).WillOnce(Return(S_OK));
    EXPECT_EQ(httpMod->OnSendResponse(&httpCtx, nullptr), RQ_NOTIFICATION_FINISH_REQUEST);
  }
  {
    MockHttpContext httpCtx;
    MockHttpRequest httpReq;
    MockEventProvider eventProvider;
    EXPECT_CALL(httpCtx, GetRequest()).WillRepeatedly(Return(&httpReq));
    EXPECT_CALL(httpCtx, GetApplication()).WillRepeatedly(Return(&app));
    EXPECT_CALL(httpCtx, GetMetadata()).WillRepeatedly(Return(&rootMetadata));

    EXPECT_CALL(httpReq, SetHeader(Matcher<PCSTR>(StrEq(k_injection_pending_header)), _, _, _))
        .WillOnce(Return(S_OK));

    EXPECT_EQ(httpMod->OnBeginRequest(&httpCtx, &eventProvider), RQ_NOTIFICATION_CONTINUE);
  }

  mod.factory->Terminate();
  mod.global->Terminate();
}

// MissingConfig: no datadogRum section at all — should not crash or inject.
TEST(InjectionPipeline, MissingConfig) {
  // Admin manager that returns E_FAIL for all paths — no datadogRum config.
  const std::map<std::wstring, MockRumConfigElement*> empty;
  MockAdminManager adminManager(empty);
  MockServer mockServer;
  EXPECT_CALL(mockServer, GetAdminManager()).WillRepeatedly(Return(&adminManager));

  auto mod = registerModule(mockServer);

  MockAppWithContext app;
  MockAppStartProvider appStart(app);
  mod.global->OnGlobalApplicationStart(&appStart);

  auto* ctx =
      static_cast<ModuleContext*>(app.GetModuleContextContainer()->GetModuleContext(nullptr));
  ASSERT_NE(ctx, nullptr);
  EXPECT_EQ(ctx->snippet, nullptr);

  CHttpModule* httpMod = nullptr;
  mod.factory->GetHttpModule(&httpMod, nullptr);

  MockHttpContext httpCtx;
  MockHttpResponse httpResp;
  MockMetadataInfo metadata(L"MACHINE/WEBROOT/APPHOST/Default Web Site");
  PreparedResponse pr;
  const char* html = "<html><head></head><body>Hello</body></html>";
  setupHtmlResponse(httpCtx, httpResp, &app, &metadata, pr, html);

  EXPECT_EQ(httpMod->OnSendResponse(&httpCtx, nullptr), RQ_NOTIFICATION_CONTINUE);

  const std::string output(static_cast<const char*>(pr.chunk.FromMemory.pBuffer),
                           pr.chunk.FromMemory.BufferLength);
  EXPECT_EQ(output, html);

  mod.factory->Terminate();
  mod.global->Terminate();
}

// get_Collection fails on initial load — no prior snippet to keep, so snippet
// stays nullptr (no injection).
TEST(InjectionPipeline, GetCollectionFailureOnFirstLoad) {
  MockRumConfigElement config(true, {{L"applicationId", L"test-app-id"},
                                     {L"clientToken", L"test-client-token"},
                                     {L"service", L"test-service"},
                                     {L"sessionSampleRate", L"100"}});
  config.SetCollectionResult(E_OUTOFMEMORY);

  MockAdminManager adminManager(&config);
  MockServer mockServer;
  EXPECT_CALL(mockServer, GetAdminManager()).WillRepeatedly(Return(&adminManager));

  auto mod = registerModule(mockServer);

  MockAppWithContext app;
  MockAppStartProvider appStart(app);
  mod.global->OnGlobalApplicationStart(&appStart);

  auto* ctx =
      static_cast<ModuleContext*>(app.GetModuleContextContainer()->GetModuleContext(nullptr));
  ASSERT_NE(ctx, nullptr);
  // First load has no prior snippet — transient error keeps nullptr.
  EXPECT_EQ(ctx->snippet, nullptr);

  mod.factory->Terminate();
  mod.global->Terminate();
}

// get_Collection fails during config reload — existing snippet is preserved
// so injection self-heals on the next successful reload.
TEST(InjectionPipeline, GetCollectionFailurePreservesExistingConfig) {
  // First: load config successfully.
  MockRumConfigElement config(true, {{L"applicationId", L"test-app-id"},
                                     {L"clientToken", L"test-client-token"},
                                     {L"service", L"test-service"},
                                     {L"sessionSampleRate", L"100"}});

  MockAdminManager adminManager(&config);
  MockServer mockServer;
  EXPECT_CALL(mockServer, GetAdminManager()).WillRepeatedly(Return(&adminManager));

  auto mod = registerModule(mockServer);

  MockAppWithContext app(L"MACHINE/WEBROOT/APPHOST/Default Web Site");
  MockAppStartProvider appStart(app);
  mod.global->OnGlobalApplicationStart(&appStart);

  auto* ctx =
      static_cast<ModuleContext*>(app.GetModuleContextContainer()->GetModuleContext(nullptr));
  ASSERT_NE(ctx, nullptr);
  ASSERT_NE(ctx->snippet, nullptr);
  auto* original_snippet = ctx->snippet;

  // Now break get_Collection and trigger config reload.
  config.SetCollectionResult(E_OUTOFMEMORY);

  MockConfigChangeProvider configChange(L"MACHINE/WEBROOT/APPHOST/Default Web Site");
  mod.global->OnGlobalConfigurationChange(&configChange);

  // Existing snippet should be preserved — not cleared by the transient error.
  EXPECT_EQ(ctx->snippet, original_snippet);

  mod.factory->Terminate();
  mod.global->Terminate();
}

// 'enabled' attribute lookup fails on initial load — get_property cannot
// distinguish a missing attribute from a COM failure, so the read is treated
// as Error. With no prior snippet, snippet stays nullptr.
TEST(InjectionPipeline, MissingEnabledAttributeOnFirstLoad) {
  MockRumConfigElement config;
  config.SetEnabledPropertyMissing(true);

  MockAdminManager adminManager(&config);
  MockServer mockServer;
  EXPECT_CALL(mockServer, GetAdminManager()).WillRepeatedly(Return(&adminManager));

  auto mod = registerModule(mockServer);

  MockAppWithContext app;
  MockAppStartProvider appStart(app);
  mod.global->OnGlobalApplicationStart(&appStart);

  auto* ctx =
      static_cast<ModuleContext*>(app.GetModuleContextContainer()->GetModuleContext(nullptr));
  ASSERT_NE(ctx, nullptr);
  EXPECT_EQ(ctx->snippet, nullptr);

  mod.factory->Terminate();
  mod.global->Terminate();
}

// A reload that produces a config which fails make_snippet validation
// (here: sessionSampleRate=200, out of the 0..100 range) must preserve the
// previous valid snippet — operators rely on last-known-good injection
// continuing while a broken deploy is investigated.
TEST(InjectionPipeline, InvalidConfigOnReloadPreservesExistingSnippet) {
  MockRumConfigElement valid;

  MockAdminManager adminManager(&valid);
  MockServer mockServer;
  EXPECT_CALL(mockServer, GetAdminManager()).WillRepeatedly(Return(&adminManager));

  auto mod = registerModule(mockServer);

  MockAppWithContext app(L"MACHINE/WEBROOT/APPHOST/Default Web Site");
  MockAppStartProvider appStart(app);
  mod.global->OnGlobalApplicationStart(&appStart);

  auto* ctx =
      static_cast<ModuleContext*>(app.GetModuleContextContainer()->GetModuleContext(nullptr));
  ASSERT_NE(ctx, nullptr);
  ASSERT_NE(ctx->snippet, nullptr);
  auto* original_snippet = ctx->snippet;

  // Swap in a config that the SDK validator rejects. Note: this must build
  // the same way as the default constructor uses but with an out-of-range
  // sample rate so make_snippet returns a snippet with error_code != 0.
  MockRumConfigElement invalid(true, {{L"applicationId", L"test-app-id"},
                                      {L"clientToken", L"test-client-token"},
                                      {L"site", L"datadoghq.com"},
                                      {L"service", L"test-service"},
                                      {L"sessionSampleRate", L"200"}});
  adminManager.SetConfig(&invalid);

  MockConfigChangeProvider configChange(L"MACHINE/WEBROOT/APPHOST/Default Web Site");
  mod.global->OnGlobalConfigurationChange(&configChange);

  EXPECT_EQ(ctx->snippet, original_snippet);

  mod.factory->Terminate();
  mod.global->Terminate();
}

// Error semantics on the 'enabled' read must not wipe an active snippet.
TEST(InjectionPipeline, MissingEnabledAttributePreservesExistingConfig) {
  MockRumConfigElement config;

  MockAdminManager adminManager(&config);
  MockServer mockServer;
  EXPECT_CALL(mockServer, GetAdminManager()).WillRepeatedly(Return(&adminManager));

  auto mod = registerModule(mockServer);

  MockAppWithContext app(L"MACHINE/WEBROOT/APPHOST/Default Web Site");
  MockAppStartProvider appStart(app);
  mod.global->OnGlobalApplicationStart(&appStart);

  auto* ctx =
      static_cast<ModuleContext*>(app.GetModuleContextContainer()->GetModuleContext(nullptr));
  ASSERT_NE(ctx, nullptr);
  ASSERT_NE(ctx->snippet, nullptr);
  auto* original_snippet = ctx->snippet;

  // Now break the 'enabled' lookup and trigger a config reload.
  config.SetEnabledPropertyMissing(true);

  MockConfigChangeProvider configChange(L"MACHINE/WEBROOT/APPHOST/Default Web Site");
  mod.global->OnGlobalConfigurationChange(&configChange);

  EXPECT_EQ(ctx->snippet, original_snippet);

  mod.factory->Terminate();
  mod.global->Terminate();
}
