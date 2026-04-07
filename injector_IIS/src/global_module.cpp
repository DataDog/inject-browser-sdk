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

GlobalModule::GlobalModule(IHttpServer *server, DWORD server_version,
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
  if (auto error = maybe_telemetry_cfg.if_error()) {
    logger_->error(std::format("Failed to configure the telemetry module: {}",
                               error->message));
  } else {

    const datadog::tracing::Clock &clock = datadog::tracing::default_clock;
    datadog::tracing::DatadogAgentConfig agent_cfg_for_telemetry;
    auto finalized_agent_cfg_result = datadog::tracing::finalize_config(
        agent_cfg_for_telemetry, logger_, clock);
    if (auto *error = finalized_agent_cfg_result.if_error()) {
      logger_->log_error(
          error->with_prefix("Failed to finalize agent settings for "
                             "telemetry, no telemetry will be sent."));

      return;
    }
    datadog::tracing::FinalizedDatadogAgentConfig dac =
        *finalized_agent_cfg_result;

    if (!dac.http_client) {
      logger_->error(
          "HTTPClient for telemetry is null, no telemetry will be sent");
      return;
    }

    datadog::telemetry::init(*maybe_telemetry_cfg, logger_, dac.http_client,
                             dac.event_scheduler, dac.url, clock);
  }
}

void GlobalModule::update_server_log_level() {
  auto server_config = read_server_conf(*server_, *logger_);
  if (server_config.has_value()) {
    logger_->set_log_level(server_config->log_level);
  }
}

void GlobalModule::Terminate() { delete this; }

GLOBAL_NOTIFICATION_STATUS
GlobalModule::OnGlobalApplicationStart(
    IHttpApplicationStartProvider *provider) {
  assert(server_ != nullptr);

  auto ctx = std::make_unique<ModuleContext>();
  ctx->logger = logger_.get();
  ctx->server = server_;

  auto app = provider->GetApplication();
  assert(app);

  auto config_path = app->GetAppConfigPath();
  logger_->info(std::format("Parsing configuration \"{}\" for app (id: {})",
                            wstring_to_utf8(config_path),
                            wstring_to_utf8(app->GetApplicationId())));

  update_module_context(*ctx, *server_, config_path, *logger_);
  auto hr = app->GetModuleContextContainer()->SetModuleContext(ctx.get(), g_module_id);
  if (FAILED(hr)) {
    logger_->error(std::format("Failed to set app module context: {}",
                               std::system_category().message(hr)));
    return GL_NOTIFICATION_CONTINUE;
  }

  // NOTE(@dmehala): Keep a reference on the module context for
  // `OnGlobalConfigurationChange` event.
  configurations_.emplace(config_path, ctx.release());

  return GL_NOTIFICATION_CONTINUE;
}

GLOBAL_NOTIFICATION_STATUS
GlobalModule::OnGlobalApplicationStop(IHttpApplicationStopProvider *provider) {
  auto *app = provider->GetApplication();
  if (!app) {
    return GL_NOTIFICATION_CONTINUE;
  }
  auto cfg_path = app->GetAppConfigPath();
  if (!cfg_path) {
    return GL_NOTIFICATION_CONTINUE;
  }

  configurations_.erase(cfg_path);
  return GL_NOTIFICATION_CONTINUE;
}

GLOBAL_NOTIFICATION_STATUS GlobalModule::OnGlobalConfigurationChange(
    IGlobalConfigurationChangeProvider *provider) {
  assert(provider != nullptr);
  assert(server_ != nullptr);

  auto cfg_path = provider->GetChangePath();
  if (cfg_path == nullptr) {
    return GL_NOTIFICATION_CONTINUE;
  }

  logger_->info(std::format("Dispatching configuration update (\"{}\")",
                            wstring_to_utf8(cfg_path)));

  std::wstring_view change_path_view(cfg_path);
  if (change_path_view.find(L"MACHINE/WEBROOT/APPHOST") !=
      std::wstring_view::npos) {
    update_server_log_level();
  }

  // NOTE(@dmehala): There's a mismatch between the config path given in
  // `OnGlobalApplicationStart` and the one here. This path is the common parent
  // config, we need to ensure the update is sent to all applications that
  // inherit from it.
  for (const auto &[path, ctx] : configurations_) {
    if (ctx == nullptr) {
      // NOTE(@dmehala): should not happen because configuration entries are
      // removed when an application is stopped. This should be considered as a
      // bug.
      // TODO(@dmehala): Report a telemetry log.
      assert(false && "null context in configurations_ map");
      continue;
    }

    // Question(@dmehala): should the default behaviour to keep the old setting
    // if there's an issue with the configuration?
    if (std::wstring_view(path).starts_with(cfg_path)) {
      update_module_context(*ctx, *server_, path.c_str(), *logger_);
    }
  }

  return GL_NOTIFICATION_CONTINUE;
}
} // namespace datadog::rum
