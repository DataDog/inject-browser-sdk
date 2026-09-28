// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2024-Present Datadog, Inc.

#include "global_module.h"
#include "config_reader.h"
#include "module_context.h"
#include "telemetry.h"
#include <cassert>
#include <datadog/tracer_config.h>
#include <format>
#include <string>

namespace datadog::rum {

GlobalModule::GlobalModule(IHttpServer* server, DWORD server_version,
                           std::shared_ptr<Logger> logger)
    : server_(server), logger_(std::move(logger)) {
  assert(server_ != nullptr);

  update_server_log_level();

  // TODO(@dmehala): Add `rum` product in `app-started` event.
  datadog::telemetry::Configuration cfg;
  cfg.enabled = true;
  cfg.integration_name = "iis";
  cfg.integration_version = std::to_string(server_version);

  auto maybe_telemetry_cfg = datadog::telemetry::finalize_config(cfg);
  if (auto* error = maybe_telemetry_cfg.if_error()) {
    logger_->error(std::format("Failed to configure the telemetry module: {}", error->message));
  } else {
    const datadog::tracing::Clock& clock = datadog::tracing::default_clock;
    datadog::tracing::DatadogAgentConfig const agent_cfg_for_telemetry;
    auto finalized_agent_cfg_result =
        datadog::tracing::finalize_config(agent_cfg_for_telemetry, logger_, clock);
    if (auto* error = finalized_agent_cfg_result.if_error()) {
      logger_->log_error(
          error->with_prefix("Failed to finalize agent settings for "
                             "telemetry, no telemetry will be sent."));

      return;
    }
    datadog::tracing::FinalizedDatadogAgentConfig const dac = *finalized_agent_cfg_result;

    if (!dac.http_client) {
      logger_->error("HTTPClient for telemetry is null, no telemetry will be sent");
      return;
    }

    datadog::telemetry::init(*maybe_telemetry_cfg, logger_, dac.http_client, dac.event_scheduler,
                             dac.url, clock);
  }
}

void GlobalModule::update_server_log_level() {
  auto server_config = read_server_conf(*server_, *logger_);
  if (server_config.has_value()) {
    logger_->set_log_level(server_config->log_level);
  }
}

void GlobalModule::Terminate() {
  delete this;
  release_module_id_claim();
}

GLOBAL_NOTIFICATION_STATUS
GlobalModule::OnGlobalApplicationStart(IHttpApplicationStartProvider* provider) {
  PrepareApp(provider->GetApplication());
  return GL_NOTIFICATION_CONTINUE;
}

HRESULT GlobalModule::PrepareApp(IHttpApplication* app) {
  if (app == nullptr) {
    logger_->error("Application is null while preparing RUM context");
    return E_POINTER;
  }
  const auto* config_path = app->GetAppConfigPath();
  auto* app_container = app->GetModuleContextContainer();
  if (config_path == nullptr || app_container == nullptr) {
    logger_->error("Application config path or context container is null");
    return E_POINTER;
  }

  const std::lock_guard lock(configurations_mutex_);
  if (auto* existing = app_container->GetModuleContext(g_module_id); existing != nullptr) {
    configurations_.insert_or_assign(
        config_path, ConfigurationEntry{app, static_cast<ModuleContext*>(existing)});
    return S_OK;
  }

  const auto* app_id = app->GetApplicationId();
  logger_->info(std::format("Parsing configuration \"{}\" for app (id: {})",
                            wstring_to_utf8(config_path),
                            wstring_to_utf8(app_id ? app_id : L"(unknown)")));
  auto ctx = std::make_unique<ModuleContext>();
  ctx->logger = logger_.get();
  ctx->server = server_;
  update_module_context(*ctx, *server_, config_path, *logger_);

  const HRESULT hr = app_container->SetModuleContext(ctx.get(), g_module_id);
  if (hr == HRESULT_FROM_WIN32(ERROR_ALREADY_ASSIGNED)) {
    auto* winner = app_container->GetModuleContext(g_module_id);
    if (winner == nullptr) {
      logger_->error("Application RUM context race had no winner");
      return hr;
    }
    configurations_.insert_or_assign(config_path,
                                     ConfigurationEntry{app, static_cast<ModuleContext*>(winner)});
    return S_OK;
  }
  if (FAILED(hr)) {
    logger_->error(
        std::format("Failed to set app module context: {}", std::system_category().message(hr)));
    return hr;
  }

  // The container now owns the context; retain only an observer for config changes.
  auto* stored = ctx.release();
  configurations_.insert_or_assign(config_path, ConfigurationEntry{app, stored});
  return S_OK;
}

GLOBAL_NOTIFICATION_STATUS
GlobalModule::OnGlobalApplicationStop(IHttpApplicationStopProvider* provider) {
  auto* app = provider->GetApplication();
  if (!app) {
    return GL_NOTIFICATION_CONTINUE;
  }
  const auto* cfg_path = app->GetAppConfigPath();
  if (!cfg_path) {
    return GL_NOTIFICATION_CONTINUE;
  }

  // IIS owns the context. An older application can stop after a replacement
  // starts with the same config path, so retire only the matching app entry.
  const std::lock_guard lock(configurations_mutex_);
  auto it = configurations_.find(cfg_path);
  if (it != configurations_.end() && it->second.application == app) {
    it->second.context = nullptr;
  }
  return GL_NOTIFICATION_CONTINUE;
}

GLOBAL_NOTIFICATION_STATUS GlobalModule::OnGlobalConfigurationChange(
    IGlobalConfigurationChangeProvider* provider) {
  assert(provider != nullptr);
  assert(server_ != nullptr);

  const auto* cfg_path = provider->GetChangePath();
  if (cfg_path == nullptr) {
    return GL_NOTIFICATION_CONTINUE;
  }

  logger_->info(
      std::format("Dispatching configuration update (\"{}\")", wstring_to_utf8(cfg_path)));

  std::wstring_view const change_path_view(cfg_path);
  if (change_path_view.find(L"MACHINE/WEBROOT/APPHOST") != std::wstring_view::npos) {
    update_server_log_level();
  }

  // The change path is the common parent; iterate apps whose config path
  // is rooted at it. configurations_ is std::map (sorted), so lower_bound
  // skips unrelated leading entries.
  // TODO(@dmehala): Report a telemetry log when ctx is null (stopped app).
  std::wstring_view const change_path(cfg_path);
  const std::lock_guard lock(configurations_mutex_);
  for (auto it = configurations_.lower_bound(std::wstring(change_path));
       it != configurations_.end() && std::wstring_view(it->first).starts_with(change_path); ++it) {
    if (it->second.context != nullptr) {
      update_module_context(*it->second.context, *server_, it->first.c_str(), *logger_);
    }
  }

  return GL_NOTIFICATION_CONTINUE;
}
}  // namespace datadog::rum
