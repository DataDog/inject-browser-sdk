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
#include <memory>
#include <datadog/tracer_config.h>
#include <format>
#include <string>

namespace datadog::rum {

GlobalModule::GlobalModule(IHttpServer* server, DWORD server_version,
                           std::shared_ptr<Logger> logger)
    : server_(server), logger_(std::move(logger)) {
  assert(server_ != nullptr);

  update_server_log_level();
  init_telemetry(server_version);
  // After init_telemetry, so a failed read can report config_invalid: telemetry
  // emission is dropped before the subsystem is up (see telemetry.h).
  load_stable_config();
}

void GlobalModule::init_telemetry(DWORD server_version) {
  // TODO(@dmehala): Add `rum` product in `app-started` event.
  datadog::telemetry::Configuration cfg;
  cfg.enabled = true;
  cfg.integration_name = "iis";
  cfg.integration_version = std::to_string(server_version);

  auto maybe_telemetry_cfg = datadog::telemetry::finalize_config(cfg);
  if (auto* error = maybe_telemetry_cfg.if_error()) {
    logger_->error(std::format("Failed to configure the telemetry module: {}", error->message));
    return;
  }

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
  // Only now is it safe to emit; every early return above leaves the subsystem
  // uninitialised, and emitting into it would fault.
  telemetry::mark_ready();
}

void GlobalModule::load_stable_config() {
  std::unique_ptr<::StableConfig, StableConfigDeleter> config(read_stable_config());
  if (config == nullptr) {
    logger_->error("Failed to allocate stable configuration handle");
    return;
  }

  if (config->error_code != 0) {
    // A failed handle replays its error into every snippet built from it, which
    // would take down injection on sites configured purely through IIS. Drop it
    // and fall back to IIS-only behaviour instead.
    logger_->error(
        std::format("Ignoring unusable stable configuration: {}",
                    config->error_message != nullptr ? config->error_message : "(no message)"));
    telemetry::increment(telemetry::config_invalid,
                         telemetry::build_tags(std::format("error_code:{}", config->error_code),
                                               std::string{}, std::string{}));
    return;
  }

  if (config->is_empty) {
    // Keep the handle regardless: it still lets a per-site IIS overlay build a
    // snippet, and produces the quiet "nothing configured" code rather than a
    // validation error when there is no overlay either.
    logger_->debug("No stable configuration entries matched; keeping the handle for overlays");
  } else {
    logger_->info("Loaded stable configuration for RUM");
  }

  stable_config_ = std::move(config);
}

void GlobalModule::update_server_log_level() {
  auto server_config = read_server_conf(*server_, *logger_);
  if (server_config.has_value()) {
    logger_->set_log_level(server_config->log_level);
  }
}

void GlobalModule::Terminate() { delete this; }

GLOBAL_NOTIFICATION_STATUS
GlobalModule::OnGlobalApplicationStart(IHttpApplicationStartProvider* provider) {
  assert(server_ != nullptr);

  auto ctx = std::make_unique<ModuleContext>();
  ctx->logger = logger_.get();
  ctx->server = server_;
  ctx->stable_config = stable_config_.get();

  auto* app = provider->GetApplication();
  if (app == nullptr) {
    logger_->error("Application is null in OnGlobalApplicationStart");
    return GL_NOTIFICATION_CONTINUE;
  }

  const auto* config_path = app->GetAppConfigPath();
  if (!config_path) {
    logger_->error("Application config path is null, cannot initialize RUM");
    return GL_NOTIFICATION_CONTINUE;
  }
  const auto* app_id = app->GetApplicationId();
  logger_->info(std::format("Parsing configuration \"{}\" for app (id: {})",
                            wstring_to_utf8(config_path),
                            wstring_to_utf8(app_id ? app_id : L"(unknown)")));

  update_module_context(*ctx, *server_, config_path, *logger_, stable_config_.get());
  auto* app_container = app->GetModuleContextContainer();
  if (!app_container) {
    logger_->error("Module context container is null for application");
    return GL_NOTIFICATION_CONTINUE;
  }
  auto hr = app_container->SetModuleContext(ctx.get(), g_module_id);
  if (FAILED(hr)) {
    logger_->error(
        std::format("Failed to set app module context: {}", std::system_category().message(hr)));
    return GL_NOTIFICATION_CONTINUE;
  }

  // NOTE(@dmehala): Keep a reference on the module context for
  // `OnGlobalConfigurationChange` event.
  configurations_.insert_or_assign(config_path, ctx.release());

  return GL_NOTIFICATION_CONTINUE;
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

  // Null the observer pointer instead of erasing: IIS owns the ModuleContext
  // and will free it via CleanupStoredContext(). Setting to nullptr ensures
  // OnGlobalConfigurationChange won't dereference a dangling pointer if it
  // fires after stop for the same application during shutdown.
  auto it = configurations_.find(cfg_path);
  if (it != configurations_.end()) {
    it->second = nullptr;
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
  for (auto it = configurations_.lower_bound(std::wstring(change_path));
       it != configurations_.end() && std::wstring_view(it->first).starts_with(change_path); ++it) {
    if (it->second != nullptr) {
      update_module_context(*it->second, *server_, it->first.c_str(), *logger_,
                            stable_config_.get());
    }
  }

  return GL_NOTIFICATION_CONTINUE;
}
}  // namespace datadog::rum
