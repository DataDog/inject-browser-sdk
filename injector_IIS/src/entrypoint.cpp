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

  datadog::rum::g_module_id = module_info->GetId();

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
    return S_OK;
  }

  auto hr = module_info->SetGlobalNotifications(global_module,
                                                datadog::rum::GlobalModule::NotificationsMask);
  if (FAILED(hr)) {
    logger->error("Failed to register global notifications: " + std::system_category().message(hr));
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
    return S_OK;
  }

  return S_OK;
}
