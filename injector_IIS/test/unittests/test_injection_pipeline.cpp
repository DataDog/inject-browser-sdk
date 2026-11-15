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

#include "config_reader.h"
#include "entrypoint.h"
#include "global_module.h"
#include "http_module.h"
#include "module_context.h"
#include "mocks.h"
#include "gtest/gtest.h"

#include <array>
#include <cstring>
#include <functional>
#include <map>
#include <optional>
#include <string>
#include <utility>
#include <vector>

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
  bool schema_default_ = false;

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
  void SetSchemaDefault(bool value) { schema_default_ = value; }
  void SetValue(VARIANT value) {
    VariantClear(&value_);
    value_ = value;
  }

  HRESULT STDMETHODCALLTYPE get_Name(BSTR* pName) override {
    *pName = SysAllocString(name_);
    return S_OK;
  }
  HRESULT STDMETHODCALLTYPE get_Value(VARIANT* pValue) override {
    return VariantCopy(pValue, &value_);
  }
  HRESULT STDMETHODCALLTYPE put_Value(VARIANT) override { return E_NOTIMPL; }
  HRESULT STDMETHODCALLTYPE get_Schema(IAppHostPropertySchema**) override { return E_NOTIMPL; }
  HRESULT STDMETHODCALLTYPE GetMetadata(BSTR name, VARIANT* out) override {
    if (_wcsicmp(name, L"isDefaultValue") != 0) return E_NOTIMPL;
    VariantInit(out);
    out->vt = VT_BOOL;
    out->boolVal = schema_default_ ? VARIANT_TRUE : VARIANT_FALSE;
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
  std::optional<MockAppHostProperty> inject_static_files_prop_;
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
  void SetEnabledIsSchemaDefault(bool value) { enabled_prop_.SetSchemaDefault(value); }
  void SetVersionIsSchemaDefault(bool value) { version_prop_.SetSchemaDefault(value); }
  void SetVersion(int value) { version_prop_.SetValue(int_variant(value)); }
  void SetInjectionStrategy(InjectionStrategy strategy) {
    injection_strategy_prop_.emplace(L"injectionStrategy", int_variant(static_cast<int>(strategy)));
  }

  void SetInjectStaticFiles(bool inject) {
    inject_static_files_prop_.emplace(L"injectStaticFiles", bool_variant(inject));
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
    if (inject_static_files_prop_ && _wcsicmp(bstrName, L"injectStaticFiles") == 0) {
      *ppProperty = &*inject_static_files_prop_;
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
  // IIS's chunk array, with room for the chunks an injection writes back.
  std::array<HTTP_DATA_CHUNK, 8> chunks{};
  HTTP_RESPONSE raw;
};

// The entity chunks of a response whose body is split across several of them, as IIS does for a
// streamed response. The bodies are owned here, so they outlive the chunks pointing into them.
struct PreparedChunkedResponse {
  std::vector<std::string> bodies;
  // IIS's chunk array, with room for the chunks an injection writes back.
  std::vector<HTTP_DATA_CHUNK> chunks;
  HTTP_RESPONSE raw;
};

// Room the chunk arrays above leave past the response's own chunks: an injection adds at most
// three.
constexpr size_t k_spare_chunks = 4;

// IIS keeps the entity chunks in an array it manages, which WriteEntityChunkByReference appends to.
// Emulated over `raw`, whose array holds `capacity` chunks.
static void emulateEntityChunkWrites(MockHttpResponse& resp, HTTP_RESPONSE& raw, size_t capacity) {
  EXPECT_CALL(resp, WriteEntityChunkByReference(_, _))
      .WillRepeatedly(Invoke([&raw, capacity](HTTP_DATA_CHUNK* chunk, LONG position) -> HRESULT {
        if (position != -1 || raw.EntityChunkCount >= capacity) {
          return HRESULT_FROM_WIN32(ERROR_NOT_ENOUGH_MEMORY);
        }
        raw.pEntityChunks[raw.EntityChunkCount++] = *chunk;
        return S_OK;
      }));
}

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
  EXPECT_CALL(resp, GetHeader(Matcher<PCSTR>(StrEq(k_injection_header)), _))
      .WillRepeatedly(DoAll(Invoke([](_In_ PCSTR, _Out_ USHORT* len) { *len = 0; }), Return("")));
  // No announced length, unless a test announces one: its expectation, set later, wins.
  EXPECT_CALL(resp, GetHeader(Matcher<PCSTR>(StrEq("Content-Length")), _))
      .WillRepeatedly(DoAll(Invoke([](_In_ PCSTR, _Out_ USHORT* len) { *len = 0; }),
                            Return(static_cast<PCSTR>(nullptr))));

  EXPECT_CALL(resp, GetStatus(_, _, _, _, _, _, _, _, _))
      .WillRepeatedly(DoAll(Invoke([](_Out_ USHORT* pCode, _Out_ USHORT*, PCSTR*, _Out_ USHORT*,
                                      _Out_ HRESULT*, PCWSTR*, _Out_ DWORD*,
                                      IAppHostConfigException**, _Out_ BOOL*) { *pCode = 200; }),
                            Return()));
}

static void setupHtmlResponse(MockHttpContext& ctx, MockHttpResponse& resp, IHttpApplication* app,
                              MockMetadataInfo* metadata, PreparedResponse& pr, const char* html) {
  setupHtmlResponseMetadata(ctx, resp, app, metadata);

  pr.chunks[0].DataChunkType = HttpDataChunkFromMemory;
  pr.chunks[0].FromMemory.pBuffer = (PVOID)html;
  pr.chunks[0].FromMemory.BufferLength = (ULONG)strlen(html);
  pr.raw = {};
  pr.raw.EntityChunkCount = 1;
  pr.raw.pEntityChunks = pr.chunks.data();

  EXPECT_CALL(resp, GetRawHttpResponse()).WillRepeatedly(Return(&pr.raw));
  emulateEntityChunkWrites(resp, pr.raw, pr.chunks.size());
}

// Same, with the body split across one entity chunk per element of `bodies`.
static void setupChunkedHtmlResponse(MockHttpContext& ctx, MockHttpResponse& resp,
                                     IHttpApplication* app, MockMetadataInfo* metadata,
                                     PreparedChunkedResponse& pr,
                                     const std::vector<std::string>& bodies) {
  setupHtmlResponseMetadata(ctx, resp, app, metadata);

  pr.bodies = bodies;
  pr.chunks.clear();
  for (auto& body : pr.bodies) {
    HTTP_DATA_CHUNK chunk{};
    chunk.DataChunkType = HttpDataChunkFromMemory;
    chunk.FromMemory.pBuffer = (PVOID)body.data();
    chunk.FromMemory.BufferLength = (ULONG)body.size();
    pr.chunks.push_back(chunk);
  }
  pr.raw = {};
  pr.raw.EntityChunkCount = (USHORT)pr.chunks.size();
  pr.chunks.resize(pr.chunks.size() + k_spare_chunks);
  pr.raw.pEntityChunks = pr.chunks.data();

  EXPECT_CALL(resp, GetRawHttpResponse()).WillRepeatedly(Return(&pr.raw));
  emulateEntityChunkWrites(resp, pr.raw, pr.chunks.size());
}

// The body IIS would send: the entity chunks the raw response points to once the module is done,
// which may be a list the module rebuilt, at whatever length it left them.
static std::string responseBody(const HTTP_RESPONSE& raw) {
  std::string body;
  for (USHORT i = 0; i < raw.EntityChunkCount; i++) {
    const auto& chunk = raw.pEntityChunks[i];
    if (chunk.DataChunkType == HttpDataChunkFromFileHandle) {
      body += "[file]";
      continue;
    }
    body.append(static_cast<const char*>(chunk.FromMemory.pBuffer), chunk.FromMemory.BufferLength);
  }
  return body;
}

static std::string assembleResponseBody(const PreparedChunkedResponse& pr) {
  return responseBody(pr.raw);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

TEST(InjectionPipeline, FullModuleLifecycle) {
  MockRumConfigElement rumConfig;
  MockAdminManager adminManager(&rumConfig);
  MockServer mockServer;
  EXPECT_CALL(mockServer, GetAdminManager()).WillRepeatedly(Return(&adminManager));

  const auto mod = registerModule(mockServer);
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

  const std::string output = responseBody(pr.raw);
  EXPECT_NE(output.find("DD_RUM"), std::string::npos) << "Expected DD_RUM in output, got:\n"
                                                      << output;
  EXPECT_NE(output.find("test-app-id"), std::string::npos);
  EXPECT_NE(output.find("test-client-token"), std::string::npos);
  EXPECT_NE(output.find("test-service"), std::string::npos);
  EXPECT_NE(output.find("</head>"), std::string::npos);

  mod.factory->Terminate();
  mod.global->Terminate();
}

TEST(InjectionPipeline, LazyPrepareAppIsIdempotent) {
  MockRumConfigElement rumConfig;
  MockAdminManager adminManager(&rumConfig);
  MockServer mockServer;
  EXPECT_CALL(mockServer, GetAdminManager()).WillRepeatedly(Return(&adminManager));

  auto mod = registerModule(mockServer);
  auto* runtime = static_cast<GlobalModule*>(mod.global);
  MockAppWithContext app;
  ASSERT_EQ(runtime->PrepareApp(&app), S_OK);
  auto* first = app.GetModuleContextContainer()->GetModuleContext(nullptr);
  ASSERT_NE(first, nullptr);
  ASSERT_NE(static_cast<ModuleContext*>(first)->snippet, nullptr);

  ASSERT_EQ(runtime->PrepareApp(&app), S_OK);
  EXPECT_EQ(app.GetModuleContextContainer()->GetModuleContext(nullptr), first);
  MockAppStartProvider appStart(app);
  EXPECT_EQ(mod.global->OnGlobalApplicationStart(&appStart), GL_NOTIFICATION_CONTINUE);
  EXPECT_EQ(app.GetModuleContextContainer()->GetModuleContext(nullptr), first);

  mod.factory->Terminate();
  mod.global->Terminate();
}

TEST(InjectionPipeline, LazyPrepareAppUsesRaceWinner) {
  MockRumConfigElement rumConfig;
  MockAdminManager adminManager(&rumConfig);
  MockServer mockServer;
  EXPECT_CALL(mockServer, GetAdminManager()).WillRepeatedly(Return(&adminManager));

  auto mod = registerModule(mockServer);
  auto* runtime = static_cast<GlobalModule*>(mod.global);
  MockAppWithContext app;
  auto* winner = new ModuleContext;
  static_cast<MockModuleContextContainer*>(app.GetModuleContextContainer())
      ->SimulateRaceWith(winner);
  EXPECT_EQ(runtime->PrepareApp(&app), S_OK);
  EXPECT_EQ(app.GetModuleContextContainer()->GetModuleContext(nullptr), winner);

  mod.factory->Terminate();
  mod.global->Terminate();
}

// The `injectionStrategy` attribute reaches the injector, and without it the module injects right
// before `</head>`. The title separates the strategies: the head-open one injects before it,
// `</head>` after it.
TEST(InjectionPipeline, InjectionStrategyFromConfig) {
  struct TestCase {
    std::string name;
    std::optional<InjectionStrategy> configured;  // nullopt: no `injectionStrategy` attribute
    InjectionStrategy expected_strategy;
    std::string expected_before_snippet;
  };

  const std::string html =
      "<html><head><meta charset='utf-8'><title>t</title></head><body>hi</body></html>";

  const std::vector<TestCase> cases = {
      // The default: right before `</head>`.
      {"default", std::nullopt, InjectionStrategy::HeadCloseTag,
       "<html><head><meta charset='utf-8'><title>t</title>"},
      // Right after the encoding declaration instead. A response this short is held back in full
      // by the head window and only resolves at `injector_end`.
      {"head open tag", InjectionStrategy::HeadOpenTag, InjectionStrategy::HeadOpenTag,
       "<html><head><meta charset='utf-8'>"},
      {"head close tag", InjectionStrategy::HeadCloseTag, InjectionStrategy::HeadCloseTag,
       "<html><head><meta charset='utf-8'><title>t</title>"},
  };

  for (const auto& test_case : cases) {
    SCOPED_TRACE(test_case.name);

    MockRumConfigElement rumConfig;
    if (test_case.configured) {
      rumConfig.SetInjectionStrategy(*test_case.configured);
    }
    MockAdminManager adminManager(&rumConfig);
    MockServer mockServer;
    EXPECT_CALL(mockServer, GetAdminManager()).WillRepeatedly(Return(&adminManager));

    const auto mod = registerModule(mockServer);
    ASSERT_NE(mod.global, nullptr);

    MockAppWithContext app;
    MockAppStartProvider appStart(app);
    ASSERT_EQ(mod.global->OnGlobalApplicationStart(&appStart), GL_NOTIFICATION_CONTINUE);

    auto* ctx =
        static_cast<ModuleContext*>(app.GetModuleContextContainer()->GetModuleContext(nullptr));
    ASSERT_NE(ctx, nullptr);
    ASSERT_NE(ctx->snippet, nullptr);
    EXPECT_EQ(ctx->injection_strategy, test_case.expected_strategy);

    CHttpModule* httpMod = nullptr;
    ASSERT_EQ(mod.factory->GetHttpModule(&httpMod, nullptr), S_OK);

    MockHttpContext httpCtx;
    MockHttpResponse httpResp;
    MockMetadataInfo metadata(L"MACHINE/WEBROOT/APPHOST/Default Web Site");
    PreparedResponse pr;
    setupHtmlResponse(httpCtx, httpResp, &app, &metadata, pr, html.c_str());

    EXPECT_CALL(httpResp, SetHeader(Matcher<PCSTR>(StrEq(k_injection_header)), _, _, _))
        .WillOnce(Return(S_OK));
    EXPECT_CALL(httpResp, Flush(_, _, _, _)).WillOnce(Return(S_OK));

    EXPECT_EQ(httpMod->OnSendResponse(&httpCtx, nullptr), RQ_NOTIFICATION_FINISH_REQUEST);

    const std::string output = responseBody(pr.raw);
    // The snippet starts on its own line.
    const size_t snippet_at = output.find("\n<script");
    ASSERT_NE(output.find("DD_RUM"), std::string::npos) << "no snippet in output:\n" << output;
    EXPECT_EQ(output.substr(0, snippet_at), test_case.expected_before_snippet) << output;
    EXPECT_NE(output.find("<body>hi</body></html>"), std::string::npos) << output;

    mod.factory->Terminate();
    mod.global->Terminate();
  }
}

// With `headOpenTag`, the snippet goes right after the character encoding declaration, ahead of the
// rest of the head, other `<meta>` tags included. A response this short is held back in full by the
// head window and only resolves at `injector_end`.
TEST(InjectionPipeline, InjectsAfterTheEncodingDeclaration) {
  const std::string html =
      "<html><head><meta charset='utf-8'><meta name='viewport' content='x'><title>t</title></head>"
      "<body>hi</body></html>";

  MockRumConfigElement rumConfig;
  rumConfig.SetInjectionStrategy(InjectionStrategy::HeadOpenTag);
  MockAdminManager adminManager(&rumConfig);
  MockServer mockServer;
  EXPECT_CALL(mockServer, GetAdminManager()).WillRepeatedly(Return(&adminManager));

  const auto mod = registerModule(mockServer);
  ASSERT_NE(mod.global, nullptr);

  MockAppWithContext app;
  MockAppStartProvider appStart(app);
  ASSERT_EQ(mod.global->OnGlobalApplicationStart(&appStart), GL_NOTIFICATION_CONTINUE);

  CHttpModule* httpMod = nullptr;
  ASSERT_EQ(mod.factory->GetHttpModule(&httpMod, nullptr), S_OK);

  MockHttpContext httpCtx;
  MockHttpResponse httpResp;
  MockMetadataInfo metadata(L"MACHINE/WEBROOT/APPHOST/Default Web Site");
  PreparedResponse pr;
  setupHtmlResponse(httpCtx, httpResp, &app, &metadata, pr, html.c_str());

  EXPECT_CALL(httpResp, SetHeader(Matcher<PCSTR>(StrEq(k_injection_header)), _, _, _))
      .WillOnce(Return(S_OK));
  EXPECT_CALL(httpResp, Flush(_, _, _, _)).WillOnce(Return(S_OK));

  EXPECT_EQ(httpMod->OnSendResponse(&httpCtx, nullptr), RQ_NOTIFICATION_FINISH_REQUEST);

  const std::string output = responseBody(pr.raw);
  // The snippet starts on its own line.
  const size_t snippet_at = output.find("\n<script");
  ASSERT_NE(output.find("DD_RUM"), std::string::npos) << "no snippet in output:\n" << output;
  EXPECT_EQ(output.substr(0, snippet_at), "<html><head><meta charset='utf-8'>") << output;
  EXPECT_NE(output.find("<meta name='viewport' content='x'><title>t</title></head><body>hi</body>"
                        "</html>"),
            std::string::npos)
      << output;

  mod.factory->Terminate();
  mod.global->Terminate();
}

// A response that does not look like an HTML document within its first kilobyte, whatever its
// content type says: the `<head>` tag that follows is left alone, and so is the response.
TEST(InjectionPipeline, NonHtmlResponseLeavesTheResponseUntouched) {
  const std::vector<std::string> bodies = {"{\"data\": \"" + std::string(1100, 'x'),
                                           "\", \"html\": \"<head></head>\"}"};

  MockRumConfigElement rumConfig;
  MockAdminManager adminManager(&rumConfig);
  MockServer mockServer;
  EXPECT_CALL(mockServer, GetAdminManager()).WillRepeatedly(Return(&adminManager));

  const auto mod = registerModule(mockServer);
  ASSERT_NE(mod.global, nullptr);

  MockAppWithContext app;
  MockAppStartProvider appStart(app);
  ASSERT_EQ(mod.global->OnGlobalApplicationStart(&appStart), GL_NOTIFICATION_CONTINUE);

  CHttpModule* httpMod = nullptr;
  ASSERT_EQ(mod.factory->GetHttpModule(&httpMod, nullptr), S_OK);

  MockHttpContext httpCtx;
  MockHttpResponse httpResp;
  MockMetadataInfo metadata(L"MACHINE/WEBROOT/APPHOST/Default Web Site");
  PreparedChunkedResponse pr;
  setupChunkedHtmlResponse(httpCtx, httpResp, &app, &metadata, pr, bodies);

  EXPECT_CALL(httpResp, SetHeader(Matcher<PCSTR>(StrEq(k_injection_header)), _, _, _)).Times(0);
  EXPECT_CALL(httpResp, Flush(_, _, _, _)).WillOnce(Return(S_OK));

  EXPECT_EQ(httpMod->OnSendResponse(&httpCtx, nullptr), RQ_NOTIFICATION_FINISH_REQUEST);
  EXPECT_EQ(assembleResponseBody(pr), bodies[0] + bodies[1]);

  mod.factory->Terminate();
  mod.global->Terminate();
}

// A response sent in several parts, as ARR streams a large upstream response: the module sees each
// send. Flushing and finishing the request on one that more data follows would cut the response
// there, so those are left to IIS whether or not the snippet went in, and the parts after the
// first, which do not carry the headers or the start of the document, are not even looked at.
// The headers go out with the first part, so that is where the announced length grows by the
// snippet; a length that cannot be read is removed rather than left wrong.
TEST(InjectionPipeline, ResponseSentInSeveralParts) {
  enum class LengthUpdate { Unchanged, Extended, Removed };
  struct TestCase {
    std::string name;
    BOOL headers_being_sent;
    DWORD flags;
    std::string body;
    bool injects;
    std::optional<std::string> content_length;  // nullopt: the response announces none
    LengthUpdate length_update;
  };
  // Long enough to fill the head window, which a part more data follows has to.
  const std::string head = "<html><head><title>t</title></head><body>" + std::string(2000, 'x');
  const std::vector<TestCase> cases = {
      {"first part", TRUE, HTTP_SEND_RESPONSE_FLAG_MORE_DATA, head, true, "2621440",
       LengthUpdate::Extended},
      {"first part of a chunked response", TRUE, HTTP_SEND_RESPONSE_FLAG_MORE_DATA, head, true,
       std::nullopt, LengthUpdate::Unchanged},
      {"first part with an unreadable length", TRUE, HTTP_SEND_RESPONSE_FLAG_MORE_DATA, head, true,
       "12x", LengthUpdate::Removed},
      {"first part without an injection point", TRUE, HTTP_SEND_RESPONSE_FLAG_MORE_DATA,
       "<div><p>no head here</p></div>", false, "2621440", LengthUpdate::Unchanged},
      {"later part", FALSE, HTTP_SEND_RESPONSE_FLAG_MORE_DATA,
       "<p>more</p><head><title>t</title></head>", false, "2621440", LengthUpdate::Unchanged},
      {"last part", FALSE, 0, "<p>more</p><head><title>t</title></head></html>", false, "2621440",
       LengthUpdate::Unchanged},
  };

  for (const auto& test_case : cases) {
    SCOPED_TRACE(test_case.name);

    MockRumConfigElement rumConfig;
    rumConfig.SetInjectionStrategy(InjectionStrategy::HeadOpenTag);
    MockAdminManager adminManager(&rumConfig);
    MockServer mockServer;
    EXPECT_CALL(mockServer, GetAdminManager()).WillRepeatedly(Return(&adminManager));

    const auto mod = registerModule(mockServer);
    ASSERT_NE(mod.global, nullptr);

    MockAppWithContext app;
    MockAppStartProvider appStart(app);
    ASSERT_EQ(mod.global->OnGlobalApplicationStart(&appStart), GL_NOTIFICATION_CONTINUE);

    CHttpModule* httpMod = nullptr;
    ASSERT_EQ(mod.factory->GetHttpModule(&httpMod, nullptr), S_OK);

    MockHttpContext httpCtx;
    MockHttpResponse httpResp;
    MockMetadataInfo metadata(L"MACHINE/WEBROOT/APPHOST/Default Web Site");
    PreparedChunkedResponse pr;
    setupChunkedHtmlResponse(httpCtx, httpResp, &app, &metadata, pr, {test_case.body});

    const std::string announced = test_case.content_length.value_or("");
    EXPECT_CALL(httpResp, GetHeader(Matcher<PCSTR>(StrEq("Content-Length")), _))
        .WillRepeatedly(DoAll(Invoke([&announced](_In_ PCSTR, _Out_ USHORT* len) {
                                *len = static_cast<USHORT>(announced.size());
                              }),
                              Return(test_case.content_length ? announced.c_str() : nullptr)));

    EXPECT_CALL(httpResp, SetHeader(Matcher<PCSTR>(StrEq(k_injection_header)), _, _, _))
        .Times(test_case.injects ? 1 : 0)
        .WillRepeatedly(Return(S_OK));
    std::string extended_to;
    EXPECT_CALL(httpResp, SetHeader(Matcher<PCSTR>(StrEq("Content-Length")), _, _, _))
        .Times(test_case.length_update == LengthUpdate::Extended ? 1 : 0)
        .WillRepeatedly(DoAll(Invoke([&extended_to](PCSTR, PCSTR value, USHORT length, BOOL) {
                                extended_to.assign(value, length);
                              }),
                              Return(S_OK)));
    EXPECT_CALL(httpResp, DeleteHeader(Matcher<PCSTR>(StrEq("Content-Length"))))
        .Times(test_case.length_update == LengthUpdate::Removed ? 1 : 0)
        .WillRepeatedly(Return(S_OK));
    EXPECT_CALL(httpResp, Flush(_, _, _, _)).Times(0);

    MockSendResponseProvider provider(test_case.headers_being_sent, test_case.flags);
    EXPECT_EQ(httpMod->OnSendResponse(&httpCtx, &provider), RQ_NOTIFICATION_CONTINUE);

    const std::string body = assembleResponseBody(pr);
    if (test_case.injects) {
      EXPECT_NE(body.find("DD_RUM"), std::string::npos) << body;
      // Right after `<head>`, and nothing else moves.
      const size_t after_head = std::string("<html><head>").size();
      EXPECT_EQ(body.substr(0, after_head), test_case.body.substr(0, after_head));
      EXPECT_EQ(body.substr(body.size() - (test_case.body.size() - after_head)),
                test_case.body.substr(after_head));
      if (test_case.length_update == LengthUpdate::Extended) {
        // The snippet is exactly what the injection added to this part.
        const auto snippet_length = body.size() - test_case.body.size();
        EXPECT_EQ(extended_to, std::to_string(2621440 + snippet_length));
      }
    } else {
      EXPECT_EQ(body, test_case.body);
    }

    mod.factory->Terminate();
    mod.global->Terminate();
  }
}

// An application that flushes the headers before any body, as ASP.NET's `Response.Flush()` does:
// the first send has headers but no body, and the next one, with the start of the body, has no
// headers. That one is injected into, without touching the headers that are gone, unless they
// announced a length the snippet would make wrong. The sends after it are later parts.
TEST(InjectionPipeline, HeadersFlushedBeforeTheBody) {
  for (const bool announced : {false, true}) {
    SCOPED_TRACE(announced ? "with a Content-Length" : "chunked");

    MockRumConfigElement rumConfig;
    MockAdminManager adminManager(&rumConfig);
    MockServer mockServer;
    EXPECT_CALL(mockServer, GetAdminManager()).WillRepeatedly(Return(&adminManager));
    const auto mod = registerModule(mockServer);
    MockAppWithContext app;
    MockAppStartProvider appStart(app);
    ASSERT_EQ(mod.global->OnGlobalApplicationStart(&appStart), GL_NOTIFICATION_CONTINUE);
    CHttpModule* httpMod = nullptr;
    ASSERT_EQ(mod.factory->GetHttpModule(&httpMod, nullptr), S_OK);

    MockHttpContext httpCtx;
    MockHttpResponse httpResp;
    MockMetadataInfo metadata(L"MACHINE/WEBROOT/APPHOST/Default Web Site");
    MockModuleContextContainer requestContainer;
    EXPECT_CALL(httpCtx, GetModuleContextContainer()).WillRepeatedly(Return(&requestContainer));
    // The headers are gone after the first send: nothing may set any.
    EXPECT_CALL(httpResp, SetHeader(Matcher<PCSTR>(_), _, _, _)).Times(0);
    EXPECT_CALL(httpResp, DeleteHeader(Matcher<PCSTR>(_))).Times(0);
    EXPECT_CALL(httpResp, Flush(_, _, _, _)).Times(0);

    const auto send = [&](const std::vector<std::string>& bodies, BOOL headers, DWORD flags) {
      PreparedChunkedResponse pr;
      setupChunkedHtmlResponse(httpCtx, httpResp, &app, &metadata, pr, bodies);
      // A plain size rather than the string, whose copy into the lambda could throw.
      const auto length = static_cast<USHORT>(std::string_view("100000").size());
      EXPECT_CALL(httpResp, GetHeader(Matcher<PCSTR>(StrEq("Content-Length")), _))
          .WillRepeatedly(DoAll(Invoke([announced, length](_In_ PCSTR, _Out_ USHORT* len) {
                                  *len = announced ? length : 0;
                                }),
                                Return(announced ? "100000" : static_cast<PCSTR>(nullptr))));
      MockSendResponseProvider provider(headers, flags);
      EXPECT_EQ(httpMod->OnSendResponse(&httpCtx, &provider), RQ_NOTIFICATION_CONTINUE);
      return assembleResponseBody(pr);
    };

    EXPECT_EQ(send({}, TRUE, HTTP_SEND_RESPONSE_FLAG_MORE_DATA), "");
    // Past the head window: the injector decides on this part, though more data follows.
    const std::string head =
        "<html><head><title>t</title><!--" + std::string(1100, 'x') + "--></head><body>";
    const std::string body = send({head}, FALSE, HTTP_SEND_RESPONSE_FLAG_MORE_DATA);
    EXPECT_EQ(body.find("DD_RUM") != std::string::npos, !announced) << body;
    if (announced) {
      EXPECT_EQ(body, head);
    }
    // A later part: the body started already.
    const std::string later = "<p>hi</p><head></head></body></html>";
    EXPECT_EQ(send({later}, FALSE, 0), later);

    mod.factory->Terminate();
    mod.global->Terminate();
  }
}

// RUM disabled: the module does nothing for a response, however it is sent. It keeps no state on
// the request, which a send without the headers would otherwise look for, and touches no header.
TEST(InjectionPipeline, DisabledRumDoesNothing) {
  MockRumConfigElement rumConfig(false, {{L"applicationId", L"test-app-id"},
                                         {L"clientToken", L"test-client-token"},
                                         {L"site", L"datadoghq.com"}});
  MockAdminManager adminManager(&rumConfig);
  MockServer mockServer;
  EXPECT_CALL(mockServer, GetAdminManager()).WillRepeatedly(Return(&adminManager));
  const auto mod = registerModule(mockServer);
  MockAppWithContext app;
  MockAppStartProvider appStart(app);
  ASSERT_EQ(mod.global->OnGlobalApplicationStart(&appStart), GL_NOTIFICATION_CONTINUE);
  CHttpModule* httpMod = nullptr;
  ASSERT_EQ(mod.factory->GetHttpModule(&httpMod, nullptr), S_OK);

  MockHttpContext httpCtx;
  MockHttpResponse httpResp;
  MockMetadataInfo metadata(L"MACHINE/WEBROOT/APPHOST/Default Web Site");
  MockModuleContextContainer requestContainer;
  EXPECT_CALL(httpCtx, GetModuleContextContainer()).WillRepeatedly(Return(&requestContainer));
  EXPECT_CALL(httpResp, SetHeader(Matcher<PCSTR>(_), _, _, _)).Times(0);
  EXPECT_CALL(httpResp, DeleteHeader(Matcher<PCSTR>(_))).Times(0);
  EXPECT_CALL(httpResp, Flush(_, _, _, _)).Times(0);

  const auto send = [&](const std::vector<std::string>& bodies, BOOL headers, DWORD flags) {
    PreparedChunkedResponse pr;
    setupChunkedHtmlResponse(httpCtx, httpResp, &app, &metadata, pr, bodies);
    MockSendResponseProvider provider(headers, flags);
    EXPECT_EQ(httpMod->OnSendResponse(&httpCtx, &provider), RQ_NOTIFICATION_CONTINUE);
    return assembleResponseBody(pr);
  };
  const std::string page = "<html><head><title>t</title></head><body></body></html>";
  EXPECT_EQ(send({page}, TRUE, 0), page);
  EXPECT_EQ(send({}, TRUE, HTTP_SEND_RESPONSE_FLAG_MORE_DATA), "");
  EXPECT_EQ(send({page}, FALSE, HTTP_SEND_RESPONSE_FLAG_MORE_DATA), page);
  EXPECT_EQ(send({page}, FALSE, 0), page);
  EXPECT_EQ(requestContainer.GetModuleContext(nullptr), nullptr);

  mod.factory->Terminate();
  mod.global->Terminate();
}

// A module set up, with the default configuration, for one test of how it rebuilds a response.
class ModuleUnderTest {
 public:
  MockRumConfigElement rumConfig;
  MockAdminManager adminManager{&rumConfig};
  MockServer mockServer;
  RegisteredModule mod{};
  MockAppWithContext app;
  CHttpModule* httpMod = nullptr;
  MockHttpContext httpCtx;
  MockHttpResponse httpResp;
  MockMetadataInfo metadata{L"MACHINE/WEBROOT/APPHOST/Default Web Site"};

  // `configure` sets the `datadogRum` section up before the module reads it.
  explicit ModuleUnderTest(const std::function<void(MockRumConfigElement&)>& configure = {}) {
    if (configure) {
      configure(rumConfig);
    }
    EXPECT_CALL(mockServer, GetAdminManager()).WillRepeatedly(Return(&adminManager));
    mod = registerModule(mockServer);
    MockAppStartProvider appStart(app);
    EXPECT_EQ(mod.global->OnGlobalApplicationStart(&appStart), GL_NOTIFICATION_CONTINUE);
    EXPECT_EQ(mod.factory->GetHttpModule(&httpMod, nullptr), S_OK);
  }

  ModuleUnderTest(const ModuleUnderTest&) = delete;
  ModuleUnderTest& operator=(const ModuleUnderTest&) = delete;

  ~ModuleUnderTest() {
    mod.factory->Terminate();
    mod.global->Terminate();
  }
};

// Only chunks in memory can be scanned. One that is not, a file handle for instance, is served as
// it is past the injection point, and leaves the response alone when it comes before.
TEST(InjectionPipeline, ChunksNotInMemory) {
  for (const bool before : {false, true}) {
    SCOPED_TRACE(before ? "before the injection point" : "after the injection point");
    ModuleUnderTest module;
    // Long enough for the head window to be decided within the chunk in memory.
    const std::string head = "<html><head><title>t</title></head>" + std::string(2000, 'x');
    const std::vector<std::string> bodies = {head, "file"};
    PreparedChunkedResponse pr;
    setupChunkedHtmlResponse(module.httpCtx, module.httpResp, &module.app, &module.metadata, pr,
                             before ? std::vector<std::string>{"file", bodies[0]} : bodies);
    HTTP_DATA_CHUNK& file = pr.chunks[before ? 0 : 1];
    file = {};
    file.DataChunkType = HttpDataChunkFromFileHandle;
    file.FromFileHandle.FileHandle = reinterpret_cast<HANDLE>(0x1234);

    EXPECT_CALL(module.httpResp, SetHeader(Matcher<PCSTR>(StrEq(k_injection_header)), _, _, _))
        .Times(before ? 0 : 1)
        .WillRepeatedly(Return(S_OK));
    EXPECT_CALL(module.httpResp, Flush(_, _, _, _)).WillOnce(Return(S_OK));
    EXPECT_EQ(module.httpMod->OnSendResponse(&module.httpCtx, nullptr),
              RQ_NOTIFICATION_FINISH_REQUEST);

    const int file_at = before ? 0 : pr.raw.EntityChunkCount - 1;
    EXPECT_EQ(pr.raw.pEntityChunks[file_at].DataChunkType, HttpDataChunkFromFileHandle);
    EXPECT_EQ(pr.raw.pEntityChunks[file_at].FromFileHandle.FileHandle,
              reinterpret_cast<HANDLE>(0x1234));
    if (before) {
      EXPECT_EQ(pr.raw.EntityChunkCount, 2);
    } else {
      // Every chunk before the file one is in memory: the head, with the snippet in.
      std::string served;
      for (int i = 0; i < file_at; i++) {
        const auto& chunk = pr.raw.pEntityChunks[i];
        served.append(static_cast<const char*>(chunk.FromMemory.pBuffer),
                      chunk.FromMemory.BufferLength);
      }
      EXPECT_NE(served.find("DD_RUM"), std::string::npos);
      EXPECT_TRUE(served.ends_with(head.substr(head.size() - 100))) << served;
    }
  }
}

// A static file as IIS serves one: a single chunk with the whole file as its range, through a
// handle opened for overlapped I/O.
class StaticFile {
 public:
  explicit StaticFile(std::string content) : content_(std::move(content)) {
    wchar_t dir[MAX_PATH];
    wchar_t path[MAX_PATH];
    EXPECT_NE(GetTempPathW(MAX_PATH, dir), 0U);
    EXPECT_NE(GetTempFileNameW(dir, L"rum", 0, path), 0U);
    path_ = path;
    HANDLE writer =
        CreateFileW(path, GENERIC_WRITE, 0, nullptr, CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, nullptr);
    DWORD written = 0;
    EXPECT_TRUE(
        WriteFile(writer, content_.data(), static_cast<DWORD>(content_.size()), &written, nullptr));
    CloseHandle(writer);
    handle_ = CreateFileW(path, GENERIC_READ, FILE_SHARE_READ, nullptr, OPEN_EXISTING,
                          FILE_FLAG_OVERLAPPED, nullptr);
    EXPECT_NE(handle_, INVALID_HANDLE_VALUE);
  }
  StaticFile(const StaticFile&) = delete;
  StaticFile& operator=(const StaticFile&) = delete;
  ~StaticFile() {
    CloseHandle(handle_);
    DeleteFileW(path_.c_str());
  }

  HTTP_DATA_CHUNK chunk() const {
    HTTP_DATA_CHUNK chunk{};
    chunk.DataChunkType = HttpDataChunkFromFileHandle;
    chunk.FromFileHandle.FileHandle = handle_;
    chunk.FromFileHandle.ByteRange.StartingOffset.QuadPart = 0;
    chunk.FromFileHandle.ByteRange.Length.QuadPart = HTTP_BYTE_RANGE_TO_EOF;
    return chunk;
  }

  // The body IIS would send, the ranges of this file it holds read from it.
  std::string body(const HTTP_RESPONSE& raw) const {
    std::string body;
    for (USHORT i = 0; i < raw.EntityChunkCount; i++) {
      const auto& chunk = raw.pEntityChunks[i];
      if (chunk.DataChunkType == HttpDataChunkFromFileHandle) {
        EXPECT_EQ(chunk.FromFileHandle.FileHandle, handle_);
        const auto& range = chunk.FromFileHandle.ByteRange;
        body += content_.substr(range.StartingOffset.QuadPart,
                                range.Length.QuadPart == HTTP_BYTE_RANGE_TO_EOF
                                    ? std::string::npos
                                    : range.Length.QuadPart);
        continue;
      }
      body.append(static_cast<const char*>(chunk.FromMemory.pBuffer),
                  chunk.FromMemory.BufferLength);
    }
    return body;
  }

  const std::string& content() const { return content_; }

 private:
  std::string content_;
  std::wstring path_;
  HANDLE handle_ = INVALID_HANDLE_VALUE;
};

// A response serving `file`, with room in IIS's chunk array for every read of it and the splice.
static void setupStaticFileResponse(ModuleUnderTest& module, const StaticFile& file,
                                    PreparedChunkedResponse& pr) {
  setupHtmlResponseMetadata(module.httpCtx, module.httpResp, &module.app, &module.metadata);
  pr.chunks.assign(16, HTTP_DATA_CHUNK{});
  pr.chunks[0] = file.chunk();
  pr.raw = {};
  pr.raw.EntityChunkCount = 1;
  pr.raw.pEntityChunks = pr.chunks.data();
  EXPECT_CALL(module.httpResp, GetRawHttpResponse()).WillRepeatedly(Return(&pr.raw));
  emulateEntityChunkWrites(module.httpResp, pr.raw, pr.chunks.size());
}

// Serves `file` through the module, which leaves the response in `pr`.
static void serveStaticFile(ModuleUnderTest& module, const StaticFile& file,
                            PreparedChunkedResponse& pr) {
  setupStaticFileResponse(module, file, pr);
  EXPECT_CALL(module.httpResp, Flush(_, _, _, _)).WillOnce(Return(S_OK));
  EXPECT_EQ(module.httpMod->OnSendResponse(&module.httpCtx, nullptr),
            RQ_NOTIFICATION_FINISH_REQUEST);
  EXPECT_TRUE(module.httpCtx.AllocationGuardIntact()) << "wrote past the end of request memory";
}

// A static file is read as the scan reaches it, and only that far: the snippet goes before its
// `</head>`, the rest of the file still going out from it.
TEST(InjectionPipeline, StaticFiles) {
  const std::string body(size_t{100} * 1024, 'x');
  const std::string inline_script = "<script>" + std::string(size_t{40} * 1024, 'x') + "</script>";
  struct Case {
    const char* name;
    std::string content;
    // Where the part of the file still served from it starts, or nullopt for none.
    std::optional<uint64_t> rest_from;
  };
  const std::vector<Case> cases = {
      {"read whole", "<!DOCTYPE html><html><head><title>t</title></head><body>x</body></html>",
       std::nullopt},
      {"first read", "<!DOCTYPE html><html><head><title>t</title></head><body>" + body, 16 * 1024},
      // 16 KiB, as much again, then 32 KiB: the `</head>` is within the first 64 KiB.
      {"head past the first read",
       "<!DOCTYPE html><html><head>" + inline_script + "</head><body>" + body, 64 * 1024},
  };
  for (const Case& test_case : cases) {
    SCOPED_TRACE(test_case.name);
    ModuleUnderTest module;
    const StaticFile file(test_case.content);
    PreparedChunkedResponse pr;
    serveStaticFile(module, file, pr);

    const std::string served = file.body(pr.raw);
    const size_t head_end = file.content().find("</head>");
    ASSERT_GT(served.size(), file.content().size());
    const size_t snippet_length = served.size() - file.content().size();
    EXPECT_EQ(served.substr(0, head_end), file.content().substr(0, head_end));
    EXPECT_NE(served.substr(head_end, snippet_length).find("DD_RUM"), std::string::npos);
    EXPECT_EQ(served.substr(head_end + snippet_length), file.content().substr(head_end));

    const auto& last = pr.raw.pEntityChunks[pr.raw.EntityChunkCount - 1];
    if (test_case.rest_from) {
      ASSERT_EQ(last.DataChunkType, HttpDataChunkFromFileHandle);
      EXPECT_EQ(last.FromFileHandle.ByteRange.StartingOffset.QuadPart, *test_case.rest_from);
      EXPECT_EQ(last.FromFileHandle.ByteRange.Length.QuadPart,
                file.content().size() - *test_case.rest_from);
    } else {
      EXPECT_EQ(last.DataChunkType, HttpDataChunkFromMemory);
    }
  }
}

// A static file the snippet never goes into is read no further than it takes to tell, and served
// as it is.
TEST(InjectionPipeline, StaticFileNotInjectedIsReadOnlyAsFarAsNeeded) {
  ModuleUnderTest module;
  const StaticFile file("{\"page\": \"" + std::string(size_t{100} * 1024, 'x') + "\"}");
  PreparedChunkedResponse pr;
  serveStaticFile(module, file, pr);

  EXPECT_EQ(file.body(pr.raw), file.content());
  ASSERT_EQ(pr.raw.EntityChunkCount, 2);
  EXPECT_EQ(pr.raw.pEntityChunks[1].FromFileHandle.ByteRange.StartingOffset.QuadPart, 16 * 1024);
}

// With `injectStaticFiles` off, a static file is not read, and goes out from the file untouched.
// Explicitly on, it is injected as by default.
TEST(InjectionPipeline, InjectStaticFilesFromConfig) {
  const std::string html =
      "<!DOCTYPE html><html><head><title>t</title></head><body>x</body></html>";
  {
    SCOPED_TRACE("off");
    ModuleUnderTest module(
        [](MockRumConfigElement& config) { config.SetInjectStaticFiles(false); });
    const StaticFile file(html);
    PreparedChunkedResponse pr;
    setupStaticFileResponse(module, file, pr);
    EXPECT_CALL(module.httpResp, SetHeader(Matcher<PCSTR>(_), _, _, _)).Times(0);
    EXPECT_CALL(module.httpResp, Flush(_, _, _, _)).WillOnce(Return(S_OK));
    EXPECT_EQ(module.httpMod->OnSendResponse(&module.httpCtx, nullptr),
              RQ_NOTIFICATION_FINISH_REQUEST);

    ASSERT_EQ(pr.raw.EntityChunkCount, 1);
    EXPECT_EQ(pr.raw.pEntityChunks[0].DataChunkType, HttpDataChunkFromFileHandle);
    EXPECT_EQ(file.body(pr.raw), html);
  }
  {
    SCOPED_TRACE("on");
    ModuleUnderTest module([](MockRumConfigElement& config) { config.SetInjectStaticFiles(true); });
    const StaticFile file(html);
    PreparedChunkedResponse pr;
    serveStaticFile(module, file, pr);
    EXPECT_NE(file.body(pr.raw).find("DD_RUM"), std::string::npos);
  }
}

// A 206 carries a range of the document, which the snippet would corrupt: it is left alone.
TEST(InjectionPipeline, PartialContentIsNotInjected) {
  ModuleUnderTest module;
  const StaticFile file("<!DOCTYPE html><html><head><title>t</title></head><body>x</body></html>");
  PreparedChunkedResponse pr;
  setupStaticFileResponse(module, file, pr);
  EXPECT_CALL(module.httpResp, GetStatus(_, _, _, _, _, _, _, _, _))
      .WillRepeatedly(DoAll(Invoke([](_Out_ USHORT* pCode, _Out_ USHORT*, PCSTR*, _Out_ USHORT*,
                                      _Out_ HRESULT*, PCWSTR*, _Out_ DWORD*,
                                      IAppHostConfigException**, _Out_ BOOL*) { *pCode = 206; }),
                            Return()));
  EXPECT_EQ(module.httpMod->OnSendResponse(&module.httpCtx, nullptr), RQ_NOTIFICATION_CONTINUE);

  ASSERT_EQ(pr.raw.EntityChunkCount, 1);
  EXPECT_EQ(pr.raw.pEntityChunks[0].DataChunkType, HttpDataChunkFromFileHandle);
}

// IIS refusing a chunk of the rebuilt body has the original one written back: the response is
// served untouched rather than cut short.
TEST(InjectionPipeline, RefusedChunkWriteLeavesTheResponseUntouched) {
  ModuleUnderTest module;
  const std::vector<std::string> bodies = {"<html><head><title>t</title></head>", "<body></body>"};
  PreparedChunkedResponse pr;
  setupChunkedHtmlResponse(module.httpCtx, module.httpResp, &module.app, &module.metadata, pr,
                           bodies);
  // No room past the response's own chunks.
  emulateEntityChunkWrites(module.httpResp, pr.raw, bodies.size());

  EXPECT_CALL(module.httpResp, SetHeader(Matcher<PCSTR>(StrEq(k_injection_header)), _, _, _))
      .Times(0);
  EXPECT_CALL(module.httpResp, Flush(_, _, _, _)).Times(0);
  EXPECT_EQ(module.httpMod->OnSendResponse(&module.httpCtx, nullptr), RQ_NOTIFICATION_CONTINUE);
  EXPECT_EQ(assembleResponseBody(pr), bodies[0] + bodies[1]);
}

// A first part that more data follows and that ends within the head window is not the whole
// document: the injector is not ended on it, so the snippet cannot go in ahead of a declaration the
// next part may hold.
TEST(InjectionPipeline, FirstPartShorterThanTheHeadWindowIsNotInjected) {
  ModuleUnderTest module;
  PreparedChunkedResponse pr;
  setupChunkedHtmlResponse(module.httpCtx, module.httpResp, &module.app, &module.metadata, pr,
                           {"<html><head><title>t</title>"});

  EXPECT_CALL(module.httpResp, SetHeader(Matcher<PCSTR>(StrEq(k_injection_header)), _, _, _))
      .Times(0);
  EXPECT_CALL(module.httpResp, Flush(_, _, _, _)).Times(0);
  MockSendResponseProvider provider(TRUE, HTTP_SEND_RESPONSE_FLAG_MORE_DATA);
  EXPECT_EQ(module.httpMod->OnSendResponse(&module.httpCtx, &provider), RQ_NOTIFICATION_CONTINUE);
  EXPECT_EQ(assembleResponseBody(pr), "<html><head><title>t</title>");
}

// One part of a response sent in several, through the module: `headers` for the first one, `more`
// unless it is the last. The response announces `content_length`, if any.
struct Part {
  std::string body;  // none, rather than an empty chunk, when empty
  BOOL headers;
  bool more;
  bool file = false;  // a file chunk follows the body, which serves as "[file]"
};

class PartedResponse {
 public:
  MockRumConfigElement rumConfig;
  MockAdminManager adminManager{&rumConfig};
  MockServer mockServer;
  RegisteredModule mod;
  MockAppWithContext app;
  CHttpModule* httpMod = nullptr;
  MockHttpContext httpCtx;
  MockHttpResponse httpResp;
  MockMetadataInfo metadata{L"MACHINE/WEBROOT/APPHOST/Default Web Site"};
  MockModuleContextContainer requestContainer;
  std::optional<std::string> content_length;
  std::string extended_to;  // what Content-Length was set to, if it was
  // The head window logic these tests mostly cover; set before `start`.
  InjectionStrategy strategy = InjectionStrategy::HeadOpenTag;

  void start() {
    rumConfig.SetInjectionStrategy(strategy);
    EXPECT_CALL(mockServer, GetAdminManager()).WillRepeatedly(Return(&adminManager));
    mod = registerModule(mockServer);
    MockAppStartProvider appStart(app);
    ASSERT_EQ(mod.global->OnGlobalApplicationStart(&appStart), GL_NOTIFICATION_CONTINUE);
    ASSERT_EQ(mod.factory->GetHttpModule(&httpMod, nullptr), S_OK);
    EXPECT_CALL(httpCtx, GetModuleContextContainer()).WillRepeatedly(Return(&requestContainer));
    EXPECT_CALL(httpResp, SetHeader(Matcher<PCSTR>(StrEq("Content-Length")), _, _, _))
        .WillRepeatedly(DoAll(Invoke([this](PCSTR, PCSTR value, USHORT length, BOOL) {
                                extended_to.assign(value, length);
                              }),
                              Return(S_OK)));
    EXPECT_CALL(httpResp, Flush(_, _, _, _)).Times(0);
  }

  size_t snippet_length() {
    const auto* ctx = static_cast<const ModuleContext*>(
        app.GetModuleContextContainer()->GetModuleContext(nullptr));
    return ctx->snippet->length;
  }

  // Sends `part` and returns the body IIS would send for it.
  std::string send(const Part& part) {
    PreparedChunkedResponse pr;
    std::vector<std::string> bodies;
    if (!part.body.empty()) {
      bodies.push_back(part.body);
    }
    if (part.file) {
      bodies.emplace_back("file");
    }
    setupChunkedHtmlResponse(httpCtx, httpResp, &app, &metadata, pr, bodies);
    if (part.file) {
      HTTP_DATA_CHUNK& file = pr.chunks[bodies.size() - 1];
      file = {};
      file.DataChunkType = HttpDataChunkFromFileHandle;
      file.FromFileHandle.FileHandle = reinterpret_cast<HANDLE>(0x1234);
    }
    // After the setup, whose default is no announced length.
    EXPECT_CALL(httpResp, GetHeader(Matcher<PCSTR>(StrEq("Content-Length")), _))
        .WillRepeatedly(DoAll(Invoke([this](_In_ PCSTR, _Out_ USHORT* len) {
                                *len = static_cast<USHORT>(content_length.value_or("").size());
                              }),
                              Invoke([this](_In_ PCSTR, _Out_ USHORT*) -> PCSTR {
                                return content_length ? content_length->c_str() : nullptr;
                              })));

    MockSendResponseProvider provider(part.headers,
                                      part.more ? HTTP_SEND_RESPONSE_FLAG_MORE_DATA : 0);
    EXPECT_EQ(httpMod->OnSendResponse(&httpCtx, &provider), RQ_NOTIFICATION_CONTINUE);
    EXPECT_TRUE(httpCtx.AllocationGuardIntact()) << "wrote past the end of request memory";
    return assembleResponseBody(pr);
  }

  void stop() {
    mod.factory->Terminate();
    mod.global->Terminate();
  }
};

// The first part holds no `<head>` tag but looks like an HTML document: the injector carries over
// to the next parts, and the snippet goes right after the `<head>` tag of whichever holds it. The
// headers left with the first part, so the announced length counted the snippet from there.
TEST(InjectionPipeline, InjectionCarriedOverToALaterPart) {
  PartedResponse response;
  response.content_length = "100000";
  response.start();
  EXPECT_CALL(response.httpResp, SetHeader(Matcher<PCSTR>(StrEq(k_injection_header)), _, _, _))
      .Times(0);
  EXPECT_CALL(response.httpResp, DeleteHeader(Matcher<PCSTR>(_))).Times(0);

  const Part first{"<html><!-- " + std::string(2000, 'c') + " -->", TRUE, true};
  // A `<link>` may come before the `<head>` tag; body content, `<p>`, would rule it out.
  const Part second{"<link rel=icon><head><meta name=a>", FALSE, true};
  const Part last{"</head><body>hi</body></html>", FALSE, false};

  EXPECT_EQ(response.send(first), first.body);
  EXPECT_EQ(response.extended_to, std::to_string(100000 + response.snippet_length()));

  const std::string body = response.send(second);
  const std::string before = "<link rel=icon><head>";
  EXPECT_EQ(body.size(), second.body.size() + response.snippet_length()) << body;
  EXPECT_EQ(body.substr(0, before.size()), before);
  EXPECT_NE(body.find("DD_RUM"), std::string::npos) << body;
  EXPECT_EQ(body.substr(before.size() + response.snippet_length()), "<meta name=a>");

  EXPECT_EQ(response.send(last), last.body);
  response.stop();
}

// By default, a `</head>` tag past the first part is injected before in the part that holds it,
// one split between two parts included: the part of it the first one ends in is held back and goes
// out with the next one, after the snippet. The headers left with the first part, so the announced
// length counted the snippet from there.
TEST(InjectionPipeline, HeadCloseTagCarriedOverToALaterPart) {
  PartedResponse response;
  response.strategy = InjectionStrategy::HeadCloseTag;
  response.content_length = "100000";
  response.start();
  EXPECT_CALL(response.httpResp, SetHeader(Matcher<PCSTR>(StrEq(k_injection_header)), _, _, _))
      .Times(0);
  EXPECT_CALL(response.httpResp, DeleteHeader(Matcher<PCSTR>(_))).Times(0);

  const std::string head = "<html><head><script>" + std::string(3000, 'x') + "</script>";
  const Part first{head + "</he", TRUE, true};
  const Part second{"ad><body>hi", FALSE, true};
  const Part last{"</body></html>", FALSE, false};

  EXPECT_EQ(response.send(first), head);
  EXPECT_EQ(response.extended_to, std::to_string(100000 + response.snippet_length()));

  const std::string body = response.send(second);
  EXPECT_EQ(body.size(), std::string("</head><body>hi").size() + response.snippet_length()) << body;
  EXPECT_NE(body.substr(0, response.snippet_length()).find("DD_RUM"), std::string::npos) << body;
  EXPECT_EQ(body.substr(response.snippet_length()), "</head><body>hi");

  EXPECT_EQ(response.send(last), last.body);
  response.stop();
}

// A first part shorter than the head window is held back rather than sent: the injector hands it
// back with the part that completes the window, or ends the document, and decides on the whole
// window as if it had come in one piece, the snippet then in place wherever the `<head>` is.
TEST(InjectionPipeline, ShortFirstPartIsHeldBack) {
  for (const bool head_past_the_window : {true, false}) {
    SCOPED_TRACE(head_past_the_window ? "head past the window" : "head within the window");
    PartedResponse response;
    response.content_length = "100000";
    response.start();

    const Part first{"<!DOCTYPE html><html>", TRUE, true};
    const std::string preamble =
        head_past_the_window ? "<!-- " + std::string(2000, 'c') + " -->" : "";
    const Part second{preamble + "<head><meta name=a>", FALSE, true};
    const Part last{"</head><body></body></html>", FALSE, false};

    EXPECT_EQ(response.send(first), "");
    EXPECT_EQ(response.extended_to, std::to_string(100000 + response.snippet_length()));
    const std::string body = response.send(second);
    if (head_past_the_window) {
      // The window fills, without a `<head>`: the scan goes on past it, to the one in this part.
      const std::string before = first.body + preamble + "<head>";
      EXPECT_EQ(body.size(), first.body.size() + second.body.size() + response.snippet_length())
          << body;
      EXPECT_EQ(body.substr(0, before.size()), before);
      EXPECT_EQ(body.substr(before.size() + response.snippet_length()), "<meta name=a>");
      EXPECT_EQ(response.send(last), last.body);
    } else {
      // Still within the window: held back too, until the end of the document decides.
      EXPECT_EQ(body, "");
      const std::string rest = response.send(last);
      const std::string before = first.body + "<head>";
      const std::string after = "<meta name=a>" + last.body;
      EXPECT_EQ(rest.size(), before.size() + after.size() + response.snippet_length()) << rest;
      EXPECT_EQ(rest.substr(0, before.size()), before);
      EXPECT_NE(rest.find("DD_RUM"), std::string::npos);
      EXPECT_EQ(rest.substr(before.size() + response.snippet_length()), after);
    }
    response.stop();
  }
}

// A chunk not in memory, which the injector cannot read, in a part after a held-back one: what was
// held back goes out ahead of it, nothing lost, and the last part is padded to the announced
// length.
TEST(InjectionPipeline, HeldBackBytesGoOutAheadOfAChunkNotInMemory) {
  PartedResponse response;
  response.content_length = "100000";
  response.start();

  const Part first{"<!DOCTYPE html><html>", TRUE, true};
  const Part second{"<head>", FALSE, true, true};
  const Part last{"</head></html>", FALSE, false};

  EXPECT_EQ(response.send(first), "");
  EXPECT_EQ(response.send(second), first.body + second.body + "[file]");
  EXPECT_EQ(response.send(last), last.body + std::string(response.snippet_length(), ' '));
  response.stop();
}

// The carried injector never finds where the snippet goes: the last part is padded to the length
// announced, which counted the snippet, unless the response announced none. A fragment: after an
// `<html>` tag, the `<body>` tag would close the head the browser opens, and the snippet go there.
TEST(InjectionPipeline, CarriedInjectionPadsTheLastPart) {
  for (const bool announced : {true, false}) {
    SCOPED_TRACE(announced ? "with a Content-Length" : "chunked");
    PartedResponse response;
    if (announced) {
      response.content_length = "100000";
    }
    response.start();

    const Part first{"<!-- " + std::string(2000, 'c') + " -->", TRUE, true};
    const Part second{"<div><p>no head</p>", FALSE, true};
    const Part last{"</div>", FALSE, false};

    EXPECT_EQ(response.send(first), first.body);
    EXPECT_EQ(response.send(second), second.body);
    const std::string padding = announced ? std::string(response.snippet_length(), ' ') : "";
    EXPECT_EQ(response.send(last), last.body + padding);
    response.stop();
  }
}

// A last part the injector cannot read all of, a file chunk in it: the head may close in bytes it
// never read, so the document is not ended on what it did read, which would put the snippet after
// the file, out of the head. The response is padded to the length it announced instead.
TEST(InjectionPipeline, UnreadableLastPartIsNotInjectedAtItsEnd) {
  for (const bool announced : {true, false}) {
    SCOPED_TRACE(announced ? "with a Content-Length" : "chunked");
    PartedResponse response;
    response.strategy = InjectionStrategy::HeadCloseTag;
    if (announced) {
      response.content_length = "100000";
    }
    response.start();
    EXPECT_CALL(response.httpResp, SetHeader(Matcher<PCSTR>(StrEq(k_injection_header)), _, _, _))
        .Times(0);

    const std::string head =
        "<html><head><meta charset=utf-8><script>" + std::string(3000, 'x') + "</script>";
    const Part first{head, TRUE, true};
    const Part last{"<title>t</title>", FALSE, false, true};

    EXPECT_EQ(response.send(first), head);
    const std::string padding = announced ? std::string(response.snippet_length(), ' ') : "";
    EXPECT_EQ(response.send(last), last.body + "[file]" + padding);
    response.stop();
  }
}

// The headers flushed alone before the body: the first send with a body carries the injector over
// like any first part, and a `<head>` in a later one is injected after, all without touching the
// headers that are gone. Chunked: a length they announced would have ruled the injection out.
TEST(InjectionPipeline, CarriedOverAfterTheHeadersWentAlone) {
  PartedResponse response;
  response.start();
  EXPECT_CALL(response.httpResp, SetHeader(Matcher<PCSTR>(_), _, _, _)).Times(0);
  EXPECT_CALL(response.httpResp, DeleteHeader(Matcher<PCSTR>(_))).Times(0);

  const Part headers{"", TRUE, true};
  const Part first{"<html><!-- " + std::string(2000, 'c') + " -->", FALSE, true};
  // A `<link>` may come before the `<head>` tag; body content, `<p>`, would rule it out.
  const Part second{"<link rel=icon><head><meta name=a>", FALSE, true};
  const Part last{"</head><body>hi</body></html>", FALSE, false};

  EXPECT_EQ(response.send(headers), "");
  EXPECT_EQ(response.send(first), first.body);
  const std::string body = response.send(second);
  const std::string before = "<link rel=icon><head>";
  EXPECT_EQ(body.size(), second.body.size() + response.snippet_length()) << body;
  EXPECT_EQ(body.substr(0, before.size()), before);
  EXPECT_NE(body.find("DD_RUM"), std::string::npos) << body;
  // Not padded either: no length was announced.
  EXPECT_EQ(response.send(last), last.body);
  response.stop();
}

// Without HTML evidence in the first part, nothing is carried over: the later `<head>` is left
// alone, and so is the announced length.
TEST(InjectionPipeline, NoInjectionCarriedOverWithoutHtmlEvidence) {
  PartedResponse response;
  response.content_length = "100000";
  response.start();

  const Part first{"{\"data\": \"" + std::string(2000, 'x'), TRUE, true};
  const Part last{"\", \"html\": \"<head></head>\"}", FALSE, false};

  EXPECT_EQ(response.send(first), first.body);
  EXPECT_EQ(response.extended_to, "");
  EXPECT_EQ(response.send(last), last.body);
  response.stop();
}

// The carried injector owns a copy of the snippet: a configuration reload freeing the configured
// one while the response is still being sent does not affect it.
TEST(InjectionPipeline, CarriedInjectionOutlivesAConfigurationReload) {
  PartedResponse response;
  response.content_length = "100000";
  response.start();

  const Part first{"<html><!-- " + std::string(2000, 'c') + " -->", TRUE, true};
  const Part last{"<head><title>t</title></head></html>", FALSE, false};

  EXPECT_EQ(response.send(first), first.body);
  const size_t snippet_length = response.snippet_length();

  // What a reload does to the snippet of the context in use: the per-location one the module
  // cached, or else the application's.
  IHttpStoredContext* location =
      response.metadata.GetModuleContextContainer()->GetModuleContext(nullptr);
  auto* ctx = static_cast<ModuleContext*>(
      location != nullptr ? location
                          : response.app.GetModuleContextContainer()->GetModuleContext(nullptr));
  Snippet* configured = std::exchange(ctx->snippet, nullptr);
  snippet_cleanup(configured);

  const std::string body = response.send(last);
  EXPECT_EQ(body.size(), last.body.size() + snippet_length) << body;
  EXPECT_EQ(body.substr(0, std::string("<head>").size()), "<head>");
  EXPECT_NE(body.find("DD_RUM"), std::string::npos) << body;
  response.stop();
}

// A request from an injector downstream, a proxy in front that injects unless the response says it
// is injected already: only the first part can say so, so the snippet never goes into a later one.
// The first part is still injected, the injection header protecting it.
TEST(InjectionPipeline, DownstreamInjectorKeepsLaterPartsUninjected) {
  for (const bool head_in_first_part : {true, false}) {
    SCOPED_TRACE(head_in_first_part ? "head in the first part" : "head in a later part");
    PartedResponse response;
    response.content_length = "100000";
    response.start();

    MockHttpRequest httpReq;
    MockEventProvider eventProvider;
    EXPECT_CALL(response.httpCtx, GetRequest()).WillRepeatedly(Return(&httpReq));
    EXPECT_CALL(httpReq, GetHeader(Matcher<PCSTR>(StrEq(k_injection_pending_header)), _))
        .WillRepeatedly(
            DoAll(Invoke([](_In_ PCSTR, _Out_ USHORT* len) { *len = 1; }), Return("1")));
    EXPECT_CALL(httpReq, SetHeader(Matcher<PCSTR>(StrEq(k_injection_pending_header)), _, _, _))
        .WillRepeatedly(Return(S_OK));
    EXPECT_CALL(response.httpCtx, GetApplication()).WillRepeatedly(Return(&response.app));
    EXPECT_CALL(response.httpCtx, GetMetadata()).WillRepeatedly(Return(&response.metadata));
    EXPECT_EQ(response.httpMod->OnBeginRequest(&response.httpCtx, &eventProvider),
              RQ_NOTIFICATION_CONTINUE);

    EXPECT_CALL(response.httpResp, SetHeader(Matcher<PCSTR>(StrEq(k_injection_header)), _, _, _))
        .Times(head_in_first_part ? 1 : 0)
        .WillRepeatedly(Return(S_OK));

    const std::string filler(2000, 'c');
    const Part first{head_in_first_part ? "<html><head>" + filler : "<html><!-- " + filler + " -->",
                     TRUE, true};
    const Part last{"<head><title>t</title></head></html>", FALSE, false};

    const std::string first_body = response.send(first);
    EXPECT_EQ(first_body.find("DD_RUM") != std::string::npos, head_in_first_part) << first_body;
    EXPECT_EQ(response.extended_to,
              head_in_first_part ? std::to_string(100000 + response.snippet_length()) : "");
    EXPECT_EQ(response.send(last), last.body);
    response.stop();
  }
}

// The single send of a whole response is still flushed and finished, with the snippet in, and the
// length it announces grown by exactly the snippet.
TEST(InjectionPipeline, ResponseSentAtOnceIsFlushed) {
  MockRumConfigElement rumConfig;
  MockAdminManager adminManager(&rumConfig);
  MockServer mockServer;
  EXPECT_CALL(mockServer, GetAdminManager()).WillRepeatedly(Return(&adminManager));

  const auto mod = registerModule(mockServer);
  ASSERT_NE(mod.global, nullptr);

  MockAppWithContext app;
  MockAppStartProvider appStart(app);
  ASSERT_EQ(mod.global->OnGlobalApplicationStart(&appStart), GL_NOTIFICATION_CONTINUE);

  CHttpModule* httpMod = nullptr;
  ASSERT_EQ(mod.factory->GetHttpModule(&httpMod, nullptr), S_OK);

  MockHttpContext httpCtx;
  MockHttpResponse httpResp;
  MockMetadataInfo metadata(L"MACHINE/WEBROOT/APPHOST/Default Web Site");
  PreparedChunkedResponse pr;
  setupChunkedHtmlResponse(httpCtx, httpResp, &app, &metadata, pr,
                           {"<html><head><title>t</title></head><body>hi</body></html>"});

  const std::string announced = std::to_string(pr.bodies[0].size());
  EXPECT_CALL(httpResp, GetHeader(Matcher<PCSTR>(StrEq("Content-Length")), _))
      .WillRepeatedly(DoAll(Invoke([&announced](_In_ PCSTR, _Out_ USHORT* len) {
                              *len = static_cast<USHORT>(announced.size());
                            }),
                            Return(announced.c_str())));
  std::string extended_to;
  EXPECT_CALL(httpResp, SetHeader(Matcher<PCSTR>(StrEq("Content-Length")), _, _, _))
      .WillOnce(DoAll(Invoke([&extended_to](PCSTR, PCSTR value, USHORT length, BOOL) {
                        extended_to.assign(value, length);
                      }),
                      Return(S_OK)));
  EXPECT_CALL(httpResp, SetHeader(Matcher<PCSTR>(StrEq(k_injection_header)), _, _, _))
      .WillOnce(Return(S_OK));
  EXPECT_CALL(httpResp, DeleteHeader(Matcher<PCSTR>(_))).Times(0);
  EXPECT_CALL(httpResp, Flush(_, _, _, _)).WillOnce(Return(S_OK));

  MockSendResponseProvider provider(TRUE, 0);
  EXPECT_EQ(httpMod->OnSendResponse(&httpCtx, &provider), RQ_NOTIFICATION_FINISH_REQUEST);
  const std::string body = assembleResponseBody(pr);
  EXPECT_NE(body.find("DD_RUM"), std::string::npos);
  EXPECT_EQ(extended_to, std::to_string(body.size()));

  mod.factory->Terminate();
  mod.global->Terminate();
}

// A response with no injection point at all, held back in full by the head window: nothing is
// injected, so the queued chunks are served as they are, without the padding `injector_end` would
// add to keep the length constant.
TEST(InjectionPipeline, UninjectedBufferedResponseLeavesTheResponseUntouched) {
  // A fragment: a `<body>` tag would be evidence of a document, the snippet going before it.
  const std::vector<std::string> bodies = {"<div><pre>hel", "lo"};

  MockRumConfigElement rumConfig;
  MockAdminManager adminManager(&rumConfig);
  MockServer mockServer;
  EXPECT_CALL(mockServer, GetAdminManager()).WillRepeatedly(Return(&adminManager));

  const auto mod = registerModule(mockServer);
  ASSERT_NE(mod.global, nullptr);

  MockAppWithContext app;
  MockAppStartProvider appStart(app);
  ASSERT_EQ(mod.global->OnGlobalApplicationStart(&appStart), GL_NOTIFICATION_CONTINUE);

  CHttpModule* httpMod = nullptr;
  ASSERT_EQ(mod.factory->GetHttpModule(&httpMod, nullptr), S_OK);

  MockHttpContext httpCtx;
  MockHttpResponse httpResp;
  MockMetadataInfo metadata(L"MACHINE/WEBROOT/APPHOST/Default Web Site");
  PreparedChunkedResponse pr;
  setupChunkedHtmlResponse(httpCtx, httpResp, &app, &metadata, pr, bodies);

  EXPECT_CALL(httpResp, SetHeader(Matcher<PCSTR>(StrEq(k_injection_header)), _, _, _)).Times(0);
  EXPECT_CALL(httpResp, Flush(_, _, _, _)).WillOnce(Return(S_OK));

  EXPECT_EQ(httpMod->OnSendResponse(&httpCtx, nullptr), RQ_NOTIFICATION_FINISH_REQUEST);
  EXPECT_TRUE(httpCtx.AllocationGuardIntact()) << "wrote past the end of the injection buffer";
  EXPECT_EQ(assembleResponseBody(pr), "<div><pre>hello");

  mod.factory->Terminate();
  mod.global->Terminate();
}

// The injector holds back no more than the head window: past the injection point the response
// streams, so a large one is injected without being copied. Only the rebuilt chunk list
// and the bytes the injector held (the window and the snippet) are allocated.
TEST(InjectionPipeline, LargeResponseStreamsAfterHeadWindow) {
  MockRumConfigElement rumConfig;
  rumConfig.SetInjectionStrategy(InjectionStrategy::HeadOpenTag);
  MockAdminManager adminManager(&rumConfig);
  MockServer mockServer;
  EXPECT_CALL(mockServer, GetAdminManager()).WillRepeatedly(Return(&adminManager));

  const auto mod = registerModule(mockServer);
  ASSERT_NE(mod.global, nullptr);

  MockAppWithContext app;
  MockAppStartProvider appStart(app);
  ASSERT_EQ(mod.global->OnGlobalApplicationStart(&appStart), GL_NOTIFICATION_CONTINUE);

  CHttpModule* httpMod = nullptr;
  ASSERT_EQ(mod.factory->GetHttpModule(&httpMod, nullptr), S_OK);

  // Past the 8 MiB the module used to buffer at most, in a single chunk.
  const std::string head = "<html><head>";
  std::string html = head + "<title>t</title></head><body>";
  html.resize(8 * 1024 * 1024 + 1, 'a');

  MockHttpContext httpCtx;
  MockHttpResponse httpResp;
  MockMetadataInfo metadata(L"MACHINE/WEBROOT/APPHOST/Default Web Site");
  PreparedChunkedResponse pr;
  setupChunkedHtmlResponse(httpCtx, httpResp, &app, &metadata, pr, {html});

  EXPECT_CALL(httpResp, SetHeader(Matcher<PCSTR>(StrEq(k_injection_header)), _, _, _))
      .WillOnce(Return(S_OK));
  EXPECT_CALL(httpResp, Flush(_, _, _, _)).WillOnce(Return(S_OK));

  EXPECT_EQ(httpMod->OnSendResponse(&httpCtx, nullptr), RQ_NOTIFICATION_FINISH_REQUEST);
  EXPECT_TRUE(httpCtx.AllocationGuardIntact()) << "wrote past the end of the injection buffer";
  EXPECT_LT(httpCtx.AllocatedSize(), size_t{64} * 1024) << "the response was copied";

  auto* ctx =
      static_cast<ModuleContext*>(app.GetModuleContextContainer()->GetModuleContext(nullptr));
  ASSERT_NE(ctx, nullptr);
  const std::string body = assembleResponseBody(pr);
  ASSERT_EQ(body.size(), html.size() + ctx->snippet->length);
  EXPECT_EQ(body.substr(0, head.size()), head);
  EXPECT_NE(body.find("DD_RUM"), std::string::npos);
  EXPECT_EQ(body.substr(head.size() + ctx->snippet->length), html.substr(head.size()));

  mod.factory->Terminate();
  mod.global->Terminate();
}

// The head window straddling entity chunks: whatever the injector held back from the chunks before
// the injection point comes back out with the snippet, so it must be dropped from those chunks or
// it would be served twice, and the chunks after must stay as they are.
TEST(InjectionPipeline, HeadOpenTagAcrossEntityChunks) {
  const std::string html =
      "<!DOCTYPE html><html><head><meta charset='utf-8'><title>t</title></head><body>hi</body>"
      "</html>";
  const std::string before_snippet = "<!DOCTYPE html><html><head><meta charset='utf-8'>";

  // Split into three chunks at every pair of points.
  for (size_t first = 1; first < html.size(); first += 7) {
    for (size_t second = first + 1; second < html.size(); second += 11) {
      SCOPED_TRACE("chunk boundaries at bytes " + std::to_string(first) + " and " +
                   std::to_string(second));

      MockRumConfigElement rumConfig;
      rumConfig.SetInjectionStrategy(InjectionStrategy::HeadOpenTag);
      MockAdminManager adminManager(&rumConfig);
      MockServer mockServer;
      EXPECT_CALL(mockServer, GetAdminManager()).WillRepeatedly(Return(&adminManager));

      const auto mod = registerModule(mockServer);
      ASSERT_NE(mod.global, nullptr);

      MockAppWithContext app;
      MockAppStartProvider appStart(app);
      ASSERT_EQ(mod.global->OnGlobalApplicationStart(&appStart), GL_NOTIFICATION_CONTINUE);
      auto* ctx =
          static_cast<ModuleContext*>(app.GetModuleContextContainer()->GetModuleContext(nullptr));
      ASSERT_NE(ctx, nullptr);

      CHttpModule* httpMod = nullptr;
      ASSERT_EQ(mod.factory->GetHttpModule(&httpMod, nullptr), S_OK);

      MockHttpContext httpCtx;
      MockHttpResponse httpResp;
      MockMetadataInfo metadata(L"MACHINE/WEBROOT/APPHOST/Default Web Site");
      PreparedChunkedResponse pr;
      setupChunkedHtmlResponse(
          httpCtx, httpResp, &app, &metadata, pr,
          {html.substr(0, first), html.substr(first, second - first), html.substr(second)});

      EXPECT_CALL(httpResp, SetHeader(Matcher<PCSTR>(StrEq(k_injection_header)), _, _, _))
          .WillOnce(Return(S_OK));
      EXPECT_CALL(httpResp, Flush(_, _, _, _)).WillOnce(Return(S_OK));

      EXPECT_EQ(httpMod->OnSendResponse(&httpCtx, nullptr), RQ_NOTIFICATION_FINISH_REQUEST);
      EXPECT_TRUE(httpCtx.AllocationGuardIntact()) << "wrote past the end of the injection buffer";

      const std::string body = assembleResponseBody(pr);
      ASSERT_EQ(body.size(), html.size() + ctx->snippet->length) << "body:\n" << body;
      EXPECT_EQ(body.substr(0, before_snippet.size()), before_snippet) << body;
      EXPECT_EQ(body.substr(before_snippet.size() + ctx->snippet->length),
                html.substr(before_snippet.size()))
          << body;

      mod.factory->Terminate();
      mod.global->Terminate();
    }
  }
}

// Runs `html`, split into entity chunks at `boundaries`, through the module configured with
// `strategy`, and checks the snippet went in at byte `at` of the original document with every other
// byte served once, in order. With `max_allocation`, also checks no more than that much request
// memory was allocated.
static void expectInjectedAt(const std::string& html, const std::vector<size_t>& boundaries,
                             size_t at, std::optional<size_t> max_allocation = std::nullopt,
                             InjectionStrategy strategy = InjectionStrategy::HeadOpenTag) {
  std::string trace = "chunk boundaries at bytes";
  std::vector<std::string> bodies;
  size_t start = 0;
  for (const size_t boundary : boundaries) {
    trace += " " + std::to_string(boundary);
    bodies.push_back(html.substr(start, boundary - start));
    start = boundary;
  }
  bodies.push_back(html.substr(start));
  SCOPED_TRACE(trace);

  MockRumConfigElement rumConfig;
  rumConfig.SetInjectionStrategy(strategy);
  MockAdminManager adminManager(&rumConfig);
  MockServer mockServer;
  EXPECT_CALL(mockServer, GetAdminManager()).WillRepeatedly(Return(&adminManager));

  const auto mod = registerModule(mockServer);
  ASSERT_NE(mod.global, nullptr);

  MockAppWithContext app;
  MockAppStartProvider appStart(app);
  ASSERT_EQ(mod.global->OnGlobalApplicationStart(&appStart), GL_NOTIFICATION_CONTINUE);
  auto* ctx =
      static_cast<ModuleContext*>(app.GetModuleContextContainer()->GetModuleContext(nullptr));
  ASSERT_NE(ctx, nullptr);
  ASSERT_NE(ctx->snippet, nullptr);

  CHttpModule* httpMod = nullptr;
  ASSERT_EQ(mod.factory->GetHttpModule(&httpMod, nullptr), S_OK);

  MockHttpContext httpCtx;
  MockHttpResponse httpResp;
  MockMetadataInfo metadata(L"MACHINE/WEBROOT/APPHOST/Default Web Site");
  PreparedChunkedResponse pr;
  setupChunkedHtmlResponse(httpCtx, httpResp, &app, &metadata, pr, bodies);

  EXPECT_CALL(httpResp, SetHeader(Matcher<PCSTR>(StrEq(k_injection_header)), _, _, _))
      .WillOnce(Return(S_OK));
  EXPECT_CALL(httpResp, Flush(_, _, _, _)).WillOnce(Return(S_OK));

  EXPECT_EQ(httpMod->OnSendResponse(&httpCtx, nullptr), RQ_NOTIFICATION_FINISH_REQUEST);
  EXPECT_TRUE(httpCtx.AllocationGuardIntact()) << "wrote past the end of the injection buffer";
  if (max_allocation) {
    EXPECT_LE(httpCtx.AllocatedSize(), *max_allocation) << "copied more than the head window";
  }

  const std::string body = assembleResponseBody(pr);
  const size_t snippet_length = ctx->snippet->length;
  ASSERT_EQ(body.size(), html.size() + snippet_length) << body;
  EXPECT_EQ(body.substr(0, at), html.substr(0, at)) << body;
  EXPECT_NE(body.substr(at, snippet_length).find("DD_RUM"), std::string::npos) << body;
  EXPECT_EQ(body.substr(at + snippet_length), html.substr(at)) << body;

  mod.factory->Terminate();
  mod.global->Terminate();
}

// By default, right before `</head>`, including an end tag split across entity chunks, with a large
// head ahead of it: only the part of the tag an earlier chunk holds and the snippet are copied.
TEST(InjectionPipeline, HeadCloseTagAcrossEntityChunks) {
  const std::string before = "<!DOCTYPE html><html><head><meta charset='utf-8'><script>" +
                             std::string(20000, 'x') + "</script><title>t</title>";
  const std::string html = before + "</head><body>hi</body></html>";
  const size_t tag_start = before.size();
  for (size_t first = tag_start - 3; first <= tag_start + 7; first++) {
    for (size_t second = first + 1; second <= tag_start + 8; second += 3) {
      expectInjectedAt(html, {first, second}, tag_start, size_t{16} * 1024,
                       InjectionStrategy::HeadCloseTag);
    }
  }
  expectInjectedAt(html, {}, tag_start, size_t{16} * 1024, InjectionStrategy::HeadCloseTag);
}

// The head window filled by a later chunk: the injection is found while writing, with what the
// earlier chunks contributed to the window held back and copied, the rest of the writing chunk
// referenced in place, and the chunks after it left alone. Only the window and the snippet are
// copied.
TEST(InjectionPipeline, HeadOpenTagInjectedWhenLaterChunkFillsTheWindow) {
  const std::string before = "<!DOCTYPE html><html><head><meta charset='utf-8'>";
  const std::string html =
      before + "<title>" + std::string(1500, 'x') + "</title></head><body>hi</body></html>";
  for (size_t first = 1; first < 1100; first += 97) {
    for (size_t second = first + 1; second < html.size(); second += 233) {
      expectInjectedAt(html, {first, second}, before.size(), size_t{16} * 1024);
    }
  }
}

// No `<head>` within the head window, which looks like an HTML document all the same: the injector
// streams for the `<head>` tag and injects right after it, including a tag split across the chunk
// that ends the window and the next. Nothing is held back past the window.
TEST(InjectionPipeline, HeadTagPastTheWindow) {
  const std::string html = "<html><!-- " + std::string(1100, 'c') +
                           " --><head><title>t</title></head><body>hi</body></html>";
  const size_t tag_start = html.find("<head>");
  const size_t tag_end = tag_start + std::string("<head>").size();
  for (size_t first = 1000; first < 1100; first += 13) {
    for (size_t second = tag_start - 2; second <= tag_end + 2; second++) {
      expectInjectedAt(html, {first, second}, tag_end, size_t{16} * 1024);
    }
  }
}

// A `<head>` tag past the window split across three chunks, with more chunks after the one that
// completes it.
TEST(InjectionPipeline, HeadTagAcrossThreeChunksWithFollowingChunks) {
  const std::string html = "<!DOCTYPE html><html lang='en'><!-- " + std::string(1100, 'c') +
                           " --><head lang='en'><title>t</title></head><body>hi</body><p>more</p>"
                           "</html>";
  const size_t tag_start = html.find("<head");
  const size_t tag_end = html.find('>', tag_start) + 1;
  for (size_t first = tag_start; first < tag_end; first += 2) {
    for (size_t second = first + 1; second <= tag_end; second++) {
      expectInjectedAt(html, {first, second, html.size() - 4}, tag_end);
    }
  }
}

// Without request memory for the rebuilt chunks, the response is passed through as it was.
TEST(InjectionPipeline, AllocationFailureLeavesTheResponseUntouched) {
  const std::vector<std::string> bodies = {"<html><head><title>t</title>",
                                           "</head><body>hi</body>"};

  MockRumConfigElement rumConfig;
  MockAdminManager adminManager(&rumConfig);
  MockServer mockServer;
  EXPECT_CALL(mockServer, GetAdminManager()).WillRepeatedly(Return(&adminManager));

  const auto mod = registerModule(mockServer);
  ASSERT_NE(mod.global, nullptr);

  MockAppWithContext app;
  MockAppStartProvider appStart(app);
  ASSERT_EQ(mod.global->OnGlobalApplicationStart(&appStart), GL_NOTIFICATION_CONTINUE);

  CHttpModule* httpMod = nullptr;
  ASSERT_EQ(mod.factory->GetHttpModule(&httpMod, nullptr), S_OK);

  MockHttpContext httpCtx;
  httpCtx.FailAllocations();
  MockHttpResponse httpResp;
  MockMetadataInfo metadata(L"MACHINE/WEBROOT/APPHOST/Default Web Site");
  PreparedChunkedResponse pr;
  setupChunkedHtmlResponse(httpCtx, httpResp, &app, &metadata, pr, bodies);

  EXPECT_CALL(httpResp, SetHeader(Matcher<PCSTR>(StrEq(k_injection_header)), _, _, _)).Times(0);
  EXPECT_CALL(httpResp, Flush(_, _, _, _)).Times(0);

  EXPECT_EQ(httpMod->OnSendResponse(&httpCtx, nullptr), RQ_NOTIFICATION_CONTINUE);
  EXPECT_EQ(assembleResponseBody(pr), bodies[0] + bodies[1]);

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

  const auto mod = registerModule(mockServer);

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
  const std::string output = responseBody(pr.raw);
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

  const auto mod = registerModule(mockServer);

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

    const std::string output = responseBody(pr.raw);
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

    const std::string output = responseBody(pr.raw);
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

  const auto mod = registerModule(mockServer);
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

    const std::string output = responseBody(pr.raw);
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

  const auto mod = registerModule(mockServer);
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
  const auto* app_ctx =
      static_cast<const ModuleContext*>(app.GetModuleContextContainer()->GetModuleContext(nullptr));
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

  const auto mod = registerModule(mockServer);
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

  const std::string output = responseBody(pr.raw);
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

  const auto mod = registerModule(mockServer);
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

  const auto mod = registerModule(mockServer);
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

  const auto mod = registerModule(mockServer);
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
    const std::string output = responseBody(pr.raw);
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
    const std::string output = responseBody(pr.raw);
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

  const auto mod = registerModule(mockServer);
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

  // OnBeginRequest to /api — pending header must NOT be set (location is disabled).
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

  const auto mod = registerModule(mockServer);

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

  const std::string output = responseBody(pr.raw);
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

  const auto mod = registerModule(mockServer);

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

  const auto mod = registerModule(mockServer);

  MockAppWithContext app(L"MACHINE/WEBROOT/APPHOST/Default Web Site");
  MockAppStartProvider appStart(app);
  mod.global->OnGlobalApplicationStart(&appStart);

  auto* ctx =
      static_cast<ModuleContext*>(app.GetModuleContextContainer()->GetModuleContext(nullptr));
  ASSERT_NE(ctx, nullptr);
  ASSERT_NE(ctx->snippet, nullptr);
  auto* original_snippet = ctx->snippet;

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

  const auto mod = registerModule(mockServer);

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

// A reload whose config fails make_snippet validation must preserve the
// previous valid snippet — operators rely on last-known-good injection
// continuing while a broken deploy is investigated.
TEST(InjectionPipeline, InvalidConfigOnReloadPreservesExistingSnippet) {
  MockRumConfigElement valid;

  MockAdminManager adminManager(&valid);
  MockServer mockServer;
  EXPECT_CALL(mockServer, GetAdminManager()).WillRepeatedly(Return(&adminManager));

  const auto mod = registerModule(mockServer);

  MockAppWithContext app(L"MACHINE/WEBROOT/APPHOST/Default Web Site");
  MockAppStartProvider appStart(app);
  mod.global->OnGlobalApplicationStart(&appStart);

  auto* ctx =
      static_cast<ModuleContext*>(app.GetModuleContextContainer()->GetModuleContext(nullptr));
  ASSERT_NE(ctx, nullptr);
  ASSERT_NE(ctx->snippet, nullptr);
  auto* original_snippet = ctx->snippet;

  // sessionSampleRate=200 is out of the 0..100 range → error_code 4.
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

  const auto mod = registerModule(mockServer);

  MockAppWithContext app(L"MACHINE/WEBROOT/APPHOST/Default Web Site");
  MockAppStartProvider appStart(app);
  mod.global->OnGlobalApplicationStart(&appStart);

  auto* ctx =
      static_cast<ModuleContext*>(app.GetModuleContextContainer()->GetModuleContext(nullptr));
  ASSERT_NE(ctx, nullptr);
  ASSERT_NE(ctx->snippet, nullptr);
  auto* original_snippet = ctx->snippet;

  config.SetEnabledPropertyMissing(true);

  MockConfigChangeProvider configChange(L"MACHINE/WEBROOT/APPHOST/Default Web Site");
  mod.global->OnGlobalConfigurationChange(&configChange);

  EXPECT_EQ(ctx->snippet, original_snippet);

  mod.factory->Terminate();
  mod.global->Terminate();
}

RumConfig read_default_site(MockRumConfigElement& config) {
  MockAdminManager admin(&config);
  MockServer server;
  EXPECT_CALL(server, GetAdminManager()).WillRepeatedly(Return(&admin));
  Logger logger;
  auto result = read_conf(server, L"MACHINE/WEBROOT/APPHOST/Default Web Site", logger);
  EXPECT_EQ(result.status, ReadConfStatus::Ok);
  return result.config.value_or(RumConfig{});
}

// Schema defaults are set by no web.config, so they leave stable configuration in force.
TEST(InjectionPipeline, SchemaDefaultsAreNotExplicitValues) {
  MockRumConfigElement config;
  config.SetEnabledIsSchemaDefault(true);
  config.SetVersionIsSchemaDefault(true);

  const auto result = read_default_site(config);
  EXPECT_FALSE(result.enabled);
  EXPECT_FALSE(result.version);
  EXPECT_FALSE(result.injection_strategy);
  EXPECT_FALSE(result.inject_static_files);
  EXPECT_EQ(result.sdk_options.at("applicationId"), "test-app-id");
}

// Values a web.config sets override stable configuration.
TEST(InjectionPipeline, ExplicitValuesOverrideStableConfiguration) {
  MockRumConfigElement config;
  config.SetInjectionStrategy(InjectionStrategy::HeadOpenTag);
  config.SetInjectStaticFiles(false);

  const auto result = read_default_site(config);
  EXPECT_EQ(result.enabled, true);
  EXPECT_EQ(result.version, 5);
  EXPECT_EQ(result.injection_strategy, InjectionStrategy::HeadOpenTag);
  EXPECT_EQ(result.inject_static_files, false);
}

// An option whose value does not fit its type is skipped, and the rest still configure RUM.
TEST(InjectionPipeline, InvalidOptionValueIsSkipped) {
  MockRumConfigElement config(true, {{L"applicationId", L"test-app-id"},
                                     {L"clientToken", L"test-client-token"},
                                     {L"sessionSampleRate", L"not-a-number"}});
  MockAdminManager admin(&config);
  MockServer server;
  EXPECT_CALL(server, GetAdminManager()).WillRepeatedly(Return(&admin));
  Logger logger;
  ModuleContext ctx;

  update_module_context(ctx, server, L"MACHINE/WEBROOT/APPHOST/Default Web Site", logger);

  ASSERT_NE(ctx.snippet, nullptr);
  EXPECT_EQ(ctx.snippet->error_code, 0);
  EXPECT_EQ(ctx.application_id_tag, "application_id:test-app-id");
}

// Without web.config options or stable configuration there is nothing to inject.
TEST(InjectionPipeline, NoConfigurationClearsTheSnippet) {
  MockRumConfigElement config(true, {});
  config.SetEnabledIsSchemaDefault(true);
  config.SetVersionIsSchemaDefault(true);
  MockAdminManager admin(&config);
  MockServer server;
  EXPECT_CALL(server, GetAdminManager()).WillRepeatedly(Return(&admin));
  Logger logger;
  ModuleContext ctx;

  update_module_context(ctx, server, L"MACHINE/WEBROOT/APPHOST/Default Web Site", logger);

  EXPECT_EQ(ctx.snippet, nullptr);
}
