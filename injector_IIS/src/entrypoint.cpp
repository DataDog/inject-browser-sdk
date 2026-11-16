// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2025-Present Datadog, Inc.

#include "entrypoint.h"
#include "global_module.h"
#include "http_module.h"
#include "logger.h"
#include "module_context.h"
#include "version.h"
#include <cassert>
#include <new>

__declspec(dllexport) HRESULT __stdcall RegisterModule(DWORD server_version,
                                                       IHttpModuleRegistrationInfo* module_info,
                                                       IHttpServer* server) {
  assert(server != nullptr);
  assert(module_info != nullptr);

#if defined(DD_INJECTOR_BREAK)
  __debugbreak();
#endif

  const auto logger = std::make_shared<datadog::rum::Logger>();
  logger->info(std::format("Registering Datadog RUM Injector v{}", FILE_VERSION_STRING));

  if (!datadog::rum::try_claim_module_id(module_info->GetId())) {
    logger->error("RUM DLL is already registered under another IIS module ID");
    return S_OK;
  }

  // NOTE(@dmehala): Keep a pointer on the server for reading configuration
  // when there's a configuration update.
  //
  // If we return an error from RegisterModule, IIS will retry registering this
  // module, but if it keeps failing then eventually the application pool will
  // come to a complete stop. We don't want to cause any downtime on our
  // customers' servers, so even in the case of a fatal error, we write an error
  // to the event log and return S_OK.
  auto* global_module =
      new (std::nothrow) datadog::rum::GlobalModule(server, server_version, logger);
  if (global_module == nullptr) {
    logger->error("Out of memory while creating GlobalModule");
    datadog::rum::release_module_id_claim();
    return S_OK;
  }

  auto hr = module_info->SetGlobalNotifications(global_module,
                                                datadog::rum::GlobalModule::NotificationsMask);
  if (FAILED(hr)) {
    logger->error("Failed to register global notifications: " + std::system_category().message(hr));
    global_module->Terminate();
    return S_OK;
  }

  auto* http_factory = new (std::nothrow) datadog::rum::HttpModuleFactory;
  if (http_factory == nullptr) {
    logger->error("Out of memory while creating HttpModuleFactory");
    return S_OK;
  }

  hr = module_info->SetRequestNotifications(http_factory,
                                            datadog::rum::HttpModule::NotificationsMask, 0);
  if (FAILED(hr)) {
    logger->error("Failed to register request notifications: " +
                  std::system_category().message(hr));
    http_factory->Terminate();
    return S_OK;
  }

  return S_OK;
}

// Lets the auto-inject installer reject an older RUM DLL that lacks process/registry baselines.
extern "C" __declspec(dllexport) DWORD __stdcall DatadogRumAutoConfigVersion() { return 1; }

namespace {
HRESULT __stdcall prepare_bootstrap_app(void* runtime, IHttpApplication* application) {
  if (runtime == nullptr || application == nullptr) return E_POINTER;
  try {
    return static_cast<datadog::rum::GlobalModule*>(runtime)->PrepareApp(application);
  } catch (...) {
    // Do not let C++ exceptions cross the ABI boundary into the bootstrap.
    return E_FAIL;
  }
}
}  // namespace

// The durable bootstrap calls this only after it finds an installed optional
// RUM package. It owns IIS registration; this DLL supplies the same objects as
// RegisterModule and remains pinned in the worker process until exit.
extern "C" __declspec(dllexport) HRESULT __stdcall DatadogRumCreateRuntimeV1(
    DWORD server_version, IHttpServer* server, HTTP_MODULE_ID bootstrap_module_id,
    DatadogRumRuntimeV1* runtime) {
  if (server == nullptr || runtime == nullptr || runtime->size < sizeof(DatadogRumRuntimeV1) ||
      runtime->version != kDatadogRumRuntimeAbiVersion) {
    return E_INVALIDARG;
  }
  if (!datadog::rum::try_claim_module_id(bootstrap_module_id)) {
    return HRESULT_FROM_WIN32(ERROR_ALREADY_EXISTS);
  }
  try {
    const auto logger = std::make_shared<datadog::rum::Logger>();
    auto global = std::make_unique<datadog::rum::GlobalModule>(server, server_version, logger);
    auto factory = std::make_unique<datadog::rum::HttpModuleFactory>();
    runtime->size = sizeof(DatadogRumRuntimeV1);
    runtime->version = kDatadogRumRuntimeAbiVersion;
    runtime->runtime = global.get();
    runtime->global_module = global.release();
    runtime->request_factory = factory.release();
    runtime->prepare_app = prepare_bootstrap_app;
    return S_OK;
  } catch (const std::bad_alloc&) {
    datadog::rum::release_module_id_claim();
    return E_OUTOFMEMORY;
  } catch (...) {
    datadog::rum::release_module_id_claim();
    return E_FAIL;
  }
}
