/*
 * Unless explicitly stated otherwise all files in this repository are licensed
 * under the Apache 2.0 License. This product includes software developed at
 * Datadog (https://www.datadoghq.com/).
 *
 * Copyright 2025-Present Datadog, Inc.
 */

#pragma once

#include "config_reader.h"
#include "framework.h"
#include "logger.h"
#include "module_context.h"

#include <datadog/telemetry/configuration.h>
#include <datadog/telemetry/telemetry.h>
#include <map>
#include <mutex>

namespace datadog::rum {

class GlobalModule final : public CGlobalModule {
  HTTP_MODULE_ID module_id_;
  IHttpServer* server_;
  std::shared_ptr<Logger> logger_;
  struct ConfigurationEntry {
    IHttpApplication* application;
    ModuleContext* context;  // IIS owns this pointer and calls CleanupStoredContext.
  };
  std::mutex configurations_mutex_;
  std::map<std::wstring, ConfigurationEntry> configurations_;

 public:
  GlobalModule(IHttpServer* server, DWORD server_version, std::shared_ptr<Logger> logger);
  ~GlobalModule() = default;
  void Terminate() override;

  static const DWORD NotificationsMask =
      GL_APPLICATION_START | GL_CONFIGURATION_CHANGE | GL_APPLICATION_STOP;

  GLOBAL_NOTIFICATION_STATUS
  OnGlobalApplicationStart(IHttpApplicationStartProvider* provider) override;

  // The bootstrap can load this DLL after IIS has already started an application.
  // Both entry points share one idempotent context initialization path.
  HRESULT PrepareApp(IHttpApplication* app);

  GLOBAL_NOTIFICATION_STATUS
  OnGlobalApplicationStop(IHttpApplicationStopProvider* provider) override;

  GLOBAL_NOTIFICATION_STATUS
  OnGlobalConfigurationChange(IGlobalConfigurationChangeProvider* provider) override;

 private:
  void update_server_log_level();
};

}  // namespace datadog::rum
