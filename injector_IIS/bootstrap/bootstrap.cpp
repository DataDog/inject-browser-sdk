// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2026-Present Datadog, Inc.

#include "iis_rum_runtime_abi.h"
#include "package_selector.h"

#include <windows.h>

#include <string.h>

#include <atomic>
#include <memory>
#include <mutex>
#include <new>
#include <optional>
#include <shared_mutex>
#include <utility>
#include <vector>

namespace datadog::iis_rum {
namespace {

constexpr DWORD kRequestNotifications =
    RQ_BEGIN_REQUEST | RQ_UPDATE_REQUEST_CACHE | RQ_SEND_RESPONSE;
constexpr DWORD kGlobalNotifications =
    GL_APPLICATION_START | GL_APPLICATION_STOP | GL_CONFIGURATION_CHANGE;

void diagnostic(const wchar_t* message) { OutputDebugStringW(message); }

void disable_output_cache(IHttpResponse* response) noexcept {
  if (response == nullptr) return;
  response->DisableKernelCache();
  if (auto* policy = response->GetCachePolicy()) policy->DisableUserCache();
}

// IIS output caches can serve a response without invoking any native module.
// Prevent a cached HTML response from surviving package publication/removal.
void disable_html_output_cache(IHttpContext* context) noexcept {
  if (context == nullptr) return;
  auto* response = context->GetResponse();
  if (response == nullptr) return;
  USHORT length = 0;
  const PCSTR content_type = response->GetHeader("Content-Type", &length);
  if (content_type == nullptr || length < 9 || _strnicmp(content_type, "text/html", 9) != 0) return;
  disable_output_cache(response);
}

class RuntimeState {
 public:
  RuntimeState(DWORD server_version, IHttpServer* server, HTTP_MODULE_ID module_id)
      : server_version_(server_version), server_(server), module_id_(module_id) {}

  void disable_runtime() { runtime_enabled_.store(false, std::memory_order_release); }

  // The returned runtime lives until process exit. Its DLL is intentionally
  // never unloaded: IIS may retain app/metadata contexts with its vtables.
  const DatadogRumRuntimeV1* active_runtime(IHttpApplication* application) {
    if (!runtime_enabled_.load(std::memory_order_acquire)) return nullptr;
    auto selected = select_package();
    const std::lock_guard lock(mutex_);
    if (!selected) {
      failed_package_.reset();
      return nullptr;
    }
    if (loaded_package_) {
      if (!loaded_package_->same_identity(*selected)) return nullptr;
    } else {
      if (failed_package_ && failed_package_->same_identity(*selected)) return nullptr;
      if (!load_runtime(*selected)) {
        failed_package_ = std::move(selected);
        return nullptr;
      }
    }

    // A package may be removed or its active junction retargeted while its DLL
    // is loading. Recheck before handing its objects to request processing.
    auto current = select_package();
    if (!current || !loaded_package_ || !loaded_package_->same_identity(*current)) return nullptr;
    if (application == nullptr) return nullptr;
    auto* container = application->GetModuleContextContainer();
    if (container == nullptr) return nullptr;
    if (container->GetModuleContext(module_id_) == nullptr) {
      const std::unique_lock callback_lock(callback_mutex_);
      const HRESULT hr = runtime_.prepare_app(runtime_.runtime, application);
      if (FAILED(hr) || container->GetModuleContext(module_id_) == nullptr) return nullptr;
    }
    return &runtime_;
  }

  // Global notifications reach a loaded runtime even after its package is
  // withdrawn: its map holds observer pointers to contexts IIS owns and
  // releases, so application stops must keep arriving.
  template <typename Notify>
  GLOBAL_NOTIFICATION_STATUS notify(Notify notify) {
    const std::lock_guard lock(mutex_);
    const std::unique_lock callback_lock(callback_mutex_);
    return loaded_package_ ? notify(*runtime_.global_module) : GL_NOTIFICATION_CONTINUE;
  }

  std::shared_lock<std::shared_mutex> request_callback_lock() {
    return std::shared_lock<std::shared_mutex>(callback_mutex_);
  }

  // Called while the request holds the callback read lock. The loaded
  // identity is immutable after publication, so no state mutex is needed.
  bool package_still_active() const {
    auto selected = select_package();
    return loaded_package_ && selected && loaded_package_->same_identity(*selected);
  }

 private:
  bool load_runtime(const Package& package) {
    const HMODULE library =
        LoadLibraryExW(package.dll_path.c_str(), nullptr,
                       LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_DEFAULT_DIRS);
    if (library == nullptr) {
      diagnostic(L"Datadog IIS RUM bootstrap: package DLL could not be loaded.\n");
      return false;
    }
    // Retain the library even if its entrypoint rejects our ABI. Once an
    // entrypoint has run, it may have created objects with DLL-owned vtables.
    pinned_libraries_.push_back(library);
    auto* proc = GetProcAddress(library, "DatadogRumCreateRuntimeV1");
    if (proc == nullptr) {
      diagnostic(L"Datadog IIS RUM bootstrap: package lacks runtime ABI v1.\n");
      return false;
    }
    // GetProcAddress returns FARPROC, whose generic signature differs from this
    // export; `void (*)()` is the portable intermediate between function types.
    const auto create =
        reinterpret_cast<DatadogRumCreateRuntimeV1Fn>(reinterpret_cast<void (*)()>(proc));
    DatadogRumRuntimeV1 created{};
    created.size = sizeof(created);
    created.version = kDatadogRumRuntimeAbiVersion;
    const HRESULT hr = create(server_version_, server_, module_id_, &created);
    if (FAILED(hr) || created.size != sizeof(created) ||
        created.version != kDatadogRumRuntimeAbiVersion || created.runtime == nullptr ||
        created.global_module == nullptr || created.request_factory == nullptr ||
        created.prepare_app == nullptr) {
      diagnostic(L"Datadog IIS RUM bootstrap: package runtime ABI v1 was rejected.\n");
      return false;
    }
    runtime_ = created;
    loaded_package_ = package;
    failed_package_.reset();
    return true;
  }

  DWORD server_version_;
  IHttpServer* server_;
  HTTP_MODULE_ID module_id_;
  std::atomic<bool> runtime_enabled_{true};
  std::mutex mutex_;
  std::shared_mutex callback_mutex_;
  std::optional<Package> loaded_package_;
  std::optional<Package> failed_package_;
  DatadogRumRuntimeV1 runtime_{};
  std::vector<HMODULE> pinned_libraries_;
};

class RequestProxy final : public CHttpModule {
 public:
  RequestProxy(std::shared_ptr<RuntimeState> state, IModuleAllocator* allocator)
      : state_(std::move(state)), allocator_(allocator) {}

  REQUEST_NOTIFICATION_STATUS
  OnBeginRequest(IHttpContext* context, IHttpEventProvider* provider) override {
    return forward(context,
                   [&](CHttpModule& target) { return target.OnBeginRequest(context, provider); });
  }

  REQUEST_NOTIFICATION_STATUS
  OnUpdateRequestCache(IHttpContext* context, IHttpEventProvider*) override {
    disable_html_output_cache(context);
    return RQ_NOTIFICATION_CONTINUE;
  }

  REQUEST_NOTIFICATION_STATUS
  OnSendResponse(IHttpContext* context, ISendResponseProvider* provider) override {
    disable_html_output_cache(context);
    return forward(context,
                   [&](CHttpModule& target) { return target.OnSendResponse(context, provider); });
  }

  void Dispose() override {
    if (delegate_ != nullptr) delegate_->Dispose();
    delete this;
  }

 private:
  // Hands the notification to the runtime's module while its package is still
  // active. Failures continue the request untouched.
  template <typename Call>
  REQUEST_NOTIFICATION_STATUS forward(IHttpContext* context, Call call) {
    try {
      auto* target = delegate_for(context);
      if (target == nullptr) return RQ_NOTIFICATION_CONTINUE;
      const auto callback_lock = state_->request_callback_lock();
      if (!state_->package_still_active()) return RQ_NOTIFICATION_CONTINUE;
      return call(*target);
    } catch (...) {
      return RQ_NOTIFICATION_CONTINUE;
    }
  }

  CHttpModule* delegate_for(IHttpContext* context) {
    if (context == nullptr) return nullptr;
    const auto* runtime = state_->active_runtime(context->GetApplication());
    if (runtime == nullptr) return nullptr;
    if (delegate_ == nullptr) {
      if (FAILED(runtime->request_factory->GetHttpModule(&delegate_, allocator_))) {
        delegate_ = nullptr;
      }
    }
    return delegate_;
  }

  std::shared_ptr<RuntimeState> state_;
  IModuleAllocator* allocator_;
  CHttpModule* delegate_ = nullptr;
};

class RequestFactory final : public IHttpModuleFactory {
 public:
  explicit RequestFactory(std::shared_ptr<RuntimeState> state) : state_(std::move(state)) {}

  HRESULT GetHttpModule(CHttpModule** module, IModuleAllocator* allocator) override {
    if (module == nullptr) return E_POINTER;
    *module = new (std::nothrow) RequestProxy(state_, allocator);
    return *module == nullptr ? E_OUTOFMEMORY : S_OK;
  }

  void Terminate() override { delete this; }

 private:
  std::shared_ptr<RuntimeState> state_;
};

class GlobalProxy final : public CGlobalModule {
 public:
  explicit GlobalProxy(std::shared_ptr<RuntimeState> state) : state_(std::move(state)) {}

  GLOBAL_NOTIFICATION_STATUS OnGlobalApplicationStart(
      IHttpApplicationStartProvider* provider) override {
    return forward(
        [&](CGlobalModule& module) { return module.OnGlobalApplicationStart(provider); });
  }

  GLOBAL_NOTIFICATION_STATUS OnGlobalApplicationStop(
      IHttpApplicationStopProvider* provider) override {
    return forward([&](CGlobalModule& module) { return module.OnGlobalApplicationStop(provider); });
  }

  GLOBAL_NOTIFICATION_STATUS OnGlobalConfigurationChange(
      IGlobalConfigurationChangeProvider* provider) override {
    return forward(
        [&](CGlobalModule& module) { return module.OnGlobalConfigurationChange(provider); });
  }

  void Terminate() override { delete this; }

 private:
  template <typename Notify>
  GLOBAL_NOTIFICATION_STATUS forward(Notify notify) {
    try {
      return state_->notify(notify);
    } catch (...) {
      return GL_NOTIFICATION_CONTINUE;
    }
  }

  std::shared_ptr<RuntimeState> state_;
};

}  // namespace
}  // namespace datadog::iis_rum

extern "C" __declspec(dllexport) HRESULT __stdcall RegisterModule(
    DWORD server_version, IHttpModuleRegistrationInfo* registration, IHttpServer* server) {
  if (registration == nullptr || server == nullptr) return S_OK;
  try {
    const auto state = std::make_shared<datadog::iis_rum::RuntimeState>(server_version, server,
                                                                        registration->GetId());
    auto* global = new (std::nothrow) datadog::iis_rum::GlobalProxy(state);
    if (global == nullptr) return S_OK;
    const HRESULT global_hr =
        registration->SetGlobalNotifications(global, datadog::iis_rum::kGlobalNotifications);
    if (FAILED(global_hr)) {
      delete global;
      return S_OK;
    }
    auto* factory = new (std::nothrow) datadog::iis_rum::RequestFactory(state);
    if (factory == nullptr) return S_OK;
    const HRESULT request_hr =
        registration->SetRequestNotifications(factory, datadog::iis_rum::kRequestNotifications, 0);
    if (FAILED(request_hr)) {
      delete factory;
    } else if (FAILED(registration->SetPriorityForRequestNotification(RQ_UPDATE_REQUEST_CACHE,
                                                                      PRIORITY_ALIAS_FIRST))) {
      state->disable_runtime();
      datadog::iis_rum::diagnostic(
          L"Datadog IIS RUM bootstrap: cache update priority failed; runtime disabled.\n");
    }
  } catch (...) {
    datadog::iis_rum::diagnostic(L"Datadog IIS RUM bootstrap: registration failed.\n");
  }
  // A missing optional package must not keep an IIS application pool offline.
  return S_OK;
}

extern "C" __declspec(dllexport) DWORD __stdcall DatadogIisRumBootstrapVersion() { return 1; }
