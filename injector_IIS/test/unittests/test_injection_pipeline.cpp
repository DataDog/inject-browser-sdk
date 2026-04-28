// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2024-Present Datadog, Inc.

// Integration-style test that exercises the full IIS module pipeline:
//   RegisterModule → OnGlobalApplicationStart (config reading)
//     → OnSendResponse (injection into HTML)
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
using ::testing::_;
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
  MockAppHostProperty(LPCWSTR name, VARIANT val) : value_(val) {
    name_ = SysAllocString(name);
  }
  ~MockAppHostProperty() {
    SysFreeString(name_);
    VariantClear(&value_);
  }

  ULONG STDMETHODCALLTYPE AddRef() override { return 1; }
  ULONG STDMETHODCALLTYPE Release() override { return 1; }
  HRESULT STDMETHODCALLTYPE QueryInterface(REFIID, void**) override {
    return E_NOINTERFACE;
  }

  HRESULT STDMETHODCALLTYPE get_Name(BSTR* pName) override {
    *pName = SysAllocString(name_);
    return S_OK;
  }
  HRESULT STDMETHODCALLTYPE get_Value(VARIANT* pValue) override {
    return VariantCopy(pValue, &value_);
  }
  HRESULT STDMETHODCALLTYPE put_Value(VARIANT) override { return E_NOTIMPL; }
  HRESULT STDMETHODCALLTYPE get_Schema(IAppHostPropertySchema**) override {
    return E_NOTIMPL;
  }
  HRESULT STDMETHODCALLTYPE GetMetadata(BSTR, VARIANT*) override {
    return E_NOTIMPL;
  }
  HRESULT STDMETHODCALLTYPE SetMetadata(BSTR, VARIANT) override {
    return E_NOTIMPL;
  }
  HRESULT STDMETHODCALLTYPE
  get_Exception(IAppHostPropertyException**) override {
    return E_NOTIMPL;
  }
  HRESULT STDMETHODCALLTYPE Clear() override { return E_NOTIMPL; }
  HRESULT STDMETHODCALLTYPE get_StringValue(BSTR*) override {
    return E_NOTIMPL;
  }
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
      : name_prop_(L"name", bstr_variant(name)),
        value_prop_(L"value", bstr_variant(value)) {}

  ULONG STDMETHODCALLTYPE AddRef() override { return 1; }
  ULONG STDMETHODCALLTYPE Release() override { return 1; }
  HRESULT STDMETHODCALLTYPE QueryInterface(REFIID, void**) override {
    return E_NOINTERFACE;
  }
  HRESULT STDMETHODCALLTYPE get_Name(BSTR*) override { return E_NOTIMPL; }
  HRESULT STDMETHODCALLTYPE
  get_Collection(IAppHostElementCollection**) override {
    return E_NOTIMPL;
  }
  HRESULT STDMETHODCALLTYPE
  get_ChildElements(IAppHostChildElementCollection**) override {
    return E_NOTIMPL;
  }
  HRESULT STDMETHODCALLTYPE GetMetadata(BSTR, VARIANT*) override {
    return E_NOTIMPL;
  }
  HRESULT STDMETHODCALLTYPE SetMetadata(BSTR, VARIANT) override {
    return E_NOTIMPL;
  }
  HRESULT STDMETHODCALLTYPE get_Schema(IAppHostElementSchema**) override {
    return E_NOTIMPL;
  }
  HRESULT STDMETHODCALLTYPE GetElementByName(BSTR, IAppHostElement**) override {
    return E_NOTIMPL;
  }
  HRESULT STDMETHODCALLTYPE
  GetPropertyByName(BSTR bstrName, IAppHostProperty** ppProperty) override {
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
  HRESULT STDMETHODCALLTYPE
  get_Properties(IAppHostPropertyCollection**) override {
    return E_NOTIMPL;
  }
  HRESULT STDMETHODCALLTYPE Clear() override { return E_NOTIMPL; }
  HRESULT STDMETHODCALLTYPE get_Methods(IAppHostMethodCollection**) override {
    return E_NOTIMPL;
  }
};

class MockElementCollection : public IAppHostElementCollection {
  std::vector<MockOptionElement>& elements_;

 public:
  MockElementCollection(std::vector<MockOptionElement>& elems)
      : elements_(elems) {}

  ULONG STDMETHODCALLTYPE AddRef() override { return 1; }
  ULONG STDMETHODCALLTYPE Release() override { return 1; }
  HRESULT STDMETHODCALLTYPE QueryInterface(REFIID, void**) override {
    return E_NOINTERFACE;
  }
  HRESULT STDMETHODCALLTYPE get_Count(DWORD* pCount) override {
    *pCount = static_cast<DWORD>(elements_.size());
    return S_OK;
  }
  HRESULT STDMETHODCALLTYPE get_Item(VARIANT index,
                                     IAppHostElement** ppElement) override {
    DWORD i =
        (index.vt == VT_UI4) ? index.ulVal : static_cast<DWORD>(index.iVal);
    if (i < elements_.size()) {
      *ppElement = &elements_[i];
      return S_OK;
    }
    *ppElement = nullptr;
    return E_FAIL;
  }
  HRESULT STDMETHODCALLTYPE AddElement(IAppHostElement*, INT) override {
    return E_NOTIMPL;
  }
  HRESULT STDMETHODCALLTYPE DeleteElement(VARIANT) override {
    return E_NOTIMPL;
  }
  HRESULT STDMETHODCALLTYPE Clear() override { return E_NOTIMPL; }
  HRESULT STDMETHODCALLTYPE CreateNewElement(BSTR, IAppHostElement**) override {
    return E_NOTIMPL;
  }
  HRESULT STDMETHODCALLTYPE get_Schema(IAppHostCollectionSchema**) override {
    return E_NOTIMPL;
  }
};

// Root datadogRum config element — configurable enabled, version, options.
class MockRumConfigElement : public IAppHostElement {
  MockAppHostProperty enabled_prop_;
  MockAppHostProperty version_prop_;
  std::vector<MockOptionElement> options_;
  MockElementCollection collection_;

 public:
  // Configurable constructor.
  MockRumConfigElement(bool enabled,
                       std::vector<std::pair<LPCWSTR, LPCWSTR>> opts)
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
  HRESULT STDMETHODCALLTYPE QueryInterface(REFIID, void**) override {
    return E_NOINTERFACE;
  }
  HRESULT STDMETHODCALLTYPE get_Name(BSTR*) override { return E_NOTIMPL; }
  HRESULT STDMETHODCALLTYPE
  get_Collection(IAppHostElementCollection** ppCol) override {
    *ppCol = &collection_;
    return S_OK;
  }
  HRESULT STDMETHODCALLTYPE
  get_ChildElements(IAppHostChildElementCollection**) override {
    return E_NOTIMPL;
  }
  HRESULT STDMETHODCALLTYPE GetMetadata(BSTR, VARIANT*) override {
    return E_NOTIMPL;
  }
  HRESULT STDMETHODCALLTYPE SetMetadata(BSTR, VARIANT) override {
    return E_NOTIMPL;
  }
  HRESULT STDMETHODCALLTYPE get_Schema(IAppHostElementSchema**) override {
    return E_NOTIMPL;
  }
  HRESULT STDMETHODCALLTYPE GetElementByName(BSTR, IAppHostElement**) override {
    return E_NOTIMPL;
  }
  HRESULT STDMETHODCALLTYPE
  GetPropertyByName(BSTR bstrName, IAppHostProperty** ppProperty) override {
    if (_wcsicmp(bstrName, L"enabled") == 0) {
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
  HRESULT STDMETHODCALLTYPE
  get_Properties(IAppHostPropertyCollection**) override {
    return E_NOTIMPL;
  }
  HRESULT STDMETHODCALLTYPE Clear() override { return E_NOTIMPL; }
  HRESULT STDMETHODCALLTYPE get_Methods(IAppHostMethodCollection**) override {
    return E_NOTIMPL;
  }
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

  ULONG STDMETHODCALLTYPE AddRef() override { return 1; }
  ULONG STDMETHODCALLTYPE Release() override { return 1; }
  HRESULT STDMETHODCALLTYPE QueryInterface(REFIID, void**) override {
    return E_NOINTERFACE;
  }

  HRESULT STDMETHODCALLTYPE
  GetAdminSection(BSTR bstrSectionName, BSTR bstrPath,
                  IAppHostElement** ppElement) override {
    if (_wcsicmp(bstrSectionName, L"system.webServer/datadogRum") == 0) {
      // Look up by exact path first, then fall back to default.
      std::wstring path(bstrPath ? bstrPath : L"");
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
  HRESULT STDMETHODCALLTYPE GetMetadata(BSTR, VARIANT*) override {
    return E_NOTIMPL;
  }
  HRESULT STDMETHODCALLTYPE SetMetadata(BSTR, VARIANT) override {
    return E_NOTIMPL;
  }
  HRESULT STDMETHODCALLTYPE
  get_ConfigManager(IAppHostConfigManager**) override {
    return E_NOTIMPL;
  }
};

// ---------------------------------------------------------------------------
// Mock module context container
// ---------------------------------------------------------------------------

class MockModuleContextContainer : public IHttpModuleContextContainer {
  IHttpStoredContext* ctx_ = nullptr;

 public:
  IHttpStoredContext* GetModuleContext(HTTP_MODULE_ID) override { return ctx_; }
  HRESULT SetModuleContext(IHttpStoredContext* ctx, HTTP_MODULE_ID) override {
    ctx_ = ctx;
    return S_OK;
  }
};

// Mock IMetadataInfo — provides per-request config path and context container.
class MockMetadataInfo : public IMetadataInfo {
  std::wstring meta_path_;
  MockModuleContextContainer container_;

 public:
  MockMetadataInfo(LPCWSTR metaPath) : meta_path_(metaPath) {}

  PCWSTR GetMetaPath() const override { return meta_path_.c_str(); }
  PCWSTR GetVrPath() const override { return L""; }
  IHttpTokenEntry* GetVrToken() override { return nullptr; }
  IHttpModuleContextContainer* GetModuleContextContainer() override {
    return &container_;
  }
};

// MockHttpApplication with configurable config path.
class MockAppWithContext : public IHttpApplication {
  MockModuleContextContainer container_;
  std::wstring config_path_;

 public:
  MockAppWithContext(
      LPCWSTR configPath = L"MACHINE/WEBROOT/APPHOST/Default Web Site")
      : config_path_(configPath) {}

  PCWSTR GetApplicationPhysicalPath() const override { return L"C:\\test"; }
  PCWSTR GetApplicationId() const override { return L"/"; }
  PCWSTR GetAppConfigPath() const override { return config_path_.c_str(); }
  IHttpModuleContextContainer* GetModuleContextContainer() override {
    return &container_;
  }
};

class MockAppStartProvider : public IHttpApplicationStartProvider {
  IHttpApplication& app_;

 public:
  MockAppStartProvider(IHttpApplication& app) : app_(app) {}
  IHttpApplication* GetApplication() override { return &app_; }
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

  HRESULT hr = RegisterModule(0, &moduleInfo, &server);
  EXPECT_EQ(hr, S_OK);
  return result;
}

// Set up a mock HTTP response that looks like a 200 text/html with the given
// HTML body. Returns the raw HTTP_RESPONSE (caller owns the chunk memory).
struct PreparedResponse {
  HTTP_DATA_CHUNK chunk;
  HTTP_RESPONSE raw;
};

static void setupHtmlResponse(MockHttpContext& ctx, MockHttpResponse& resp,
                              IHttpApplication* app, MockMetadataInfo* metadata,
                              PreparedResponse& pr, const char* html) {
  EXPECT_CALL(ctx, GetApplication()).WillRepeatedly(Return(app));
  EXPECT_CALL(ctx, GetResponse()).WillRepeatedly(Return(&resp));
  EXPECT_CALL(ctx, GetMetadata()).WillRepeatedly(Return(metadata));

  const char* ct = "text/html";
  EXPECT_CALL(resp, GetHeader(Matcher<PCSTR>(StrEq("Content-Type")), _))
      .WillRepeatedly(DoAll(Invoke([ct](_In_ PCSTR, _Out_ USHORT* len) {
                              *len = (USHORT)strlen(ct);
                            }),
                            Return(ct)));
  EXPECT_CALL(resp, GetHeader(Matcher<PCSTR>(StrEq("Content-Encoding")), _))
      .WillRepeatedly(DoAll(
          Invoke([](_In_ PCSTR, _Out_ USHORT* len) { *len = 0; }), Return("")));
  EXPECT_CALL(resp,
              GetHeader(Matcher<PCSTR>(StrEq("x-datadog-rum-injected")), _))
      .WillRepeatedly(DoAll(
          Invoke([](_In_ PCSTR, _Out_ USHORT* len) { *len = 0; }), Return("")));

  EXPECT_CALL(resp, GetStatus(_, _, _, _, _, _, _, _, _))
      .WillRepeatedly(DoAll(
          Invoke([](_Out_ USHORT* pCode, _Out_ USHORT*, PCSTR*, _Out_ USHORT*,
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
  EXPECT_CALL(mockServer, GetAdminManager())
      .WillRepeatedly(Return(&adminManager));

  auto mod = registerModule(mockServer);
  ASSERT_NE(mod.global, nullptr);
  ASSERT_NE(mod.factory, nullptr);

  // App start → config reading
  MockAppWithContext app;
  MockAppStartProvider appStart(app);
  EXPECT_EQ(mod.global->OnGlobalApplicationStart(&appStart),
            GL_NOTIFICATION_CONTINUE);

  auto* ctx = static_cast<ModuleContext*>(
      app.GetModuleContextContainer()->GetModuleContext(nullptr));
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

  EXPECT_CALL(
      httpResp,
      SetHeader(Matcher<PCSTR>(StrEq("x-datadog-rum-injected")), _, _, _))
      .WillOnce(Return(S_OK));
  EXPECT_CALL(httpResp, Flush(_, _, _, _)).WillOnce(Return(S_OK));

  EXPECT_EQ(httpMod->OnSendResponse(&httpCtx, nullptr),
            RQ_NOTIFICATION_FINISH_REQUEST);

  std::string output(static_cast<const char*>(pr.chunk.FromMemory.pBuffer),
                     pr.chunk.FromMemory.BufferLength);
  EXPECT_NE(output.find("DD_RUM"), std::string::npos)
      << "Expected DD_RUM in output, got:\n"
      << output;
  EXPECT_NE(output.find("test-app-id"), std::string::npos);
  EXPECT_NE(output.find("test-client-token"), std::string::npos);
  EXPECT_NE(output.find("test-service"), std::string::npos);
  EXPECT_NE(output.find("</head>"), std::string::npos);

  mod.factory->Terminate();
  mod.global->Terminate();
}

TEST(InjectionPipeline, DisabledConfig) {
  MockRumConfigElement rumConfig(false,
                                 {
                                     {L"applicationId", L"test-app-id"},
                                     {L"clientToken", L"test-client-token"},
                                 });
  MockAdminManager adminManager(&rumConfig);
  MockServer mockServer;
  EXPECT_CALL(mockServer, GetAdminManager())
      .WillRepeatedly(Return(&adminManager));

  auto mod = registerModule(mockServer);

  MockAppWithContext app;
  MockAppStartProvider appStart(app);
  mod.global->OnGlobalApplicationStart(&appStart);

  auto* ctx = static_cast<ModuleContext*>(
      app.GetModuleContextContainer()->GetModuleContext(nullptr));
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
  EXPECT_EQ(httpMod->OnSendResponse(&httpCtx, nullptr),
            RQ_NOTIFICATION_CONTINUE);

  // Body unchanged.
  std::string output(static_cast<const char*>(pr.chunk.FromMemory.pBuffer),
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
  EXPECT_CALL(mockServer, GetAdminManager())
      .WillRepeatedly(Return(&adminManager));

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
    EXPECT_CALL(
        httpResp,
        SetHeader(Matcher<PCSTR>(StrEq("x-datadog-rum-injected")), _, _, _))
        .WillOnce(Return(S_OK));
    EXPECT_CALL(httpResp, Flush(_, _, _, _)).WillOnce(Return(S_OK));

    EXPECT_EQ(httpMod->OnSendResponse(&httpCtx, nullptr),
              RQ_NOTIFICATION_FINISH_REQUEST);

    std::string output(static_cast<const char*>(pr.chunk.FromMemory.pBuffer),
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

    EXPECT_EQ(httpMod->OnSendResponse(&httpCtx, nullptr),
              RQ_NOTIFICATION_CONTINUE)
        << "/api path should skip injection (location override disabled RUM)";

    std::string output(static_cast<const char*>(pr.chunk.FromMemory.pBuffer),
                       pr.chunk.FromMemory.BufferLength);
    EXPECT_EQ(output, html) << "/api path response should be unchanged";
  }

  mod.factory->Terminate();
  mod.global->Terminate();
}
