/*
 * Unless explicitly stated otherwise all files in this repository are licensed
 * under the Apache 2.0 License. This product includes software developed at
 * Datadog (https://www.datadoghq.com/).
 *
 * Copyright 2024-Present Datadog, Inc.
 */

#pragma once

#include "config_reader.h"
#include "framework.h"
#include "logger.h"
#include "module_context.h"

#include <datadog/telemetry/configuration.h>
#include <datadog/telemetry/telemetry.h>
#include <map>
#include <memory>

namespace datadog::rum {

class GlobalModule final : public CGlobalModule {
  HTTP_MODULE_ID module_id_;
  IHttpServer* server_;
  std::shared_ptr<Logger> logger_;
  std::map<std::wstring, ModuleContext*> configurations_;
  // Stable configuration, read once from the constructor (which runs inside
  // RegisterModule, before any notification is registered, so there is no
  // publication race). Null when there was nothing usable to read.
  //
  // Every ModuleContext borrows this pointer without owning it, so the handle is
  // never replaced for the lifetime of the module — the contexts would be left
  // dangling. That also means changes to application_monitoring.yaml only take
  // effect after an app-pool recycle: the file lives outside IIS's config tree,
  // so OnGlobalConfigurationChange never fires for it.
  //
  // Sharing is safe: stable_config_snippet_create takes a const handle, does no
  // I/O and mutates nothing, so worker threads may build snippets from it
  // concurrently with OnGlobalConfigurationChange.
  std::unique_ptr<::StableConfig, StableConfigDeleter> stable_config_;

 public:
  GlobalModule(IHttpServer* server, DWORD server_version, std::shared_ptr<Logger> logger);
  ~GlobalModule() = default;
  void Terminate() override;

  static const DWORD NotificationsMask =
      GL_APPLICATION_START | GL_CONFIGURATION_CHANGE | GL_APPLICATION_STOP;

  GLOBAL_NOTIFICATION_STATUS
  OnGlobalApplicationStart(IHttpApplicationStartProvider* provider) override;

  GLOBAL_NOTIFICATION_STATUS
  OnGlobalApplicationStop(IHttpApplicationStopProvider* provider) override;

  GLOBAL_NOTIFICATION_STATUS
  OnGlobalConfigurationChange(IGlobalConfigurationChangeProvider* provider) override;

 private:
  void update_server_log_level();
  // Brings up dd-trace-cpp telemetry and arms telemetry::increment. Every
  // failure path leaves emission disabled rather than half-initialised.
  void init_telemetry(DWORD server_version);
  void load_stable_config();
};

}  // namespace datadog::rum
