// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2024-Present Datadog, Inc.

#include "http_module.h"
#include "config_reader.h"
#include "module_context.h"
#include "telemetry.h"
#include <cassert>
#include <datadog/telemetry/telemetry.h>
#include <format>
#include <memory>
#include <string>

namespace datadog::rum {
namespace {

constexpr std::string_view k_injection_header = "x-datadog-rum-injected";

std::string_view get_header(IHttpResponse &http_response,
                            std::string_view header) {
  USHORT length;
  PCSTR data = http_response.GetHeader(header.data(), &length);
  if (!data) {
    return "";
  }

  return std::string_view(data, length);
}

// Returns the per-location context if cached on the metadata container,
// otherwise falls back to the app-level context.
ModuleContext *resolve_cached_context(IHttpContext &http_context,
                                     ModuleContext &app_ctx) {
  auto *metadata = http_context.GetMetadata();
  if (!metadata) {
    app_ctx.logger->debug("Metadata unavailable, using app-level config");
    return &app_ctx;
  }
  auto *mc = metadata->GetModuleContextContainer();
  if (!mc) {
    app_ctx.logger->debug("Metadata context container unavailable, using app-level config");
    return &app_ctx;
  }
  auto *cached = mc->GetModuleContext(g_module_id);
  if (cached) {
    return static_cast<ModuleContext *>(cached);
  }
  return &app_ctx;
}

} // namespace

REQUEST_NOTIFICATION_STATUS
HttpModule::OnBeginRequest(IN IHttpContext *http_context,
                           IN IHttpEventProvider *event_provider) {
  assert(http_context != nullptr);
  assert(event_provider != nullptr);
  IHttpRequest *http_request = http_context->GetRequest();
  if (!http_request) {
    return RQ_NOTIFICATION_CONTINUE;
  }

  auto *app = http_context->GetApplication();
  if (!app) {
    return RQ_NOTIFICATION_CONTINUE;
  }
  auto *app_container = app->GetModuleContextContainer();
  if (!app_container) {
    return RQ_NOTIFICATION_CONTINUE;
  }
  auto *app_opaque = app_container->GetModuleContext(g_module_id);
  if (!app_opaque) {
    return RQ_NOTIFICATION_CONTINUE;
  }

  auto &app_ctx = *static_cast<ModuleContext *>(app_opaque);

  // Best-effort per-location check: if a prior request to this location
  // already cached its context (in OnSendResponse), use it. On the first
  // request to a location, we fall back to the app-level context.
  ModuleContext *ctx = resolve_cached_context(*http_context, app_ctx);

  if (!ctx->snippet) {
    return RQ_NOTIFICATION_CONTINUE;
  }

  HRESULT result =
      http_request->SetHeader("x-datadog-rum-injection-pending", "1", 1, TRUE);
  if (result != S_OK) {
    event_provider->SetErrorStatus(result);
    ctx->logger->error("Error setting request header: " +
                       std::system_category().message(result));
  }

  return RQ_NOTIFICATION_CONTINUE;
}

REQUEST_NOTIFICATION_STATUS
HttpModule::OnSendResponse(IN IHttpContext *http_context,
                           IN ISendResponseProvider *) {
  assert(http_context != nullptr);
  auto http_response = http_context->GetResponse();
  if (!http_response) {
    return RQ_NOTIFICATION_CONTINUE;
  }

  // Get app-level context (always needed for logger, server, tags).
  auto *app = http_context->GetApplication();
  if (!app) {
    return RQ_NOTIFICATION_CONTINUE;
  }
  auto *app_container = app->GetModuleContextContainer();
  if (!app_container) {
    return RQ_NOTIFICATION_CONTINUE;
  }
  auto *app_opaque = app_container->GetModuleContext(g_module_id);
  if (!app_opaque) {
    return RQ_NOTIFICATION_CONTINUE;
  }
  auto &app_ctx = *static_cast<ModuleContext *>(app_opaque);

  // Resolve per-<location> config. Cached on the IMetadataInfo container;
  // IIS recycles it on config change, so hot-reload works automatically.
  ModuleContext *ctx = resolve_cached_context(*http_context, app_ctx);

  // Cache miss — read config for this request's effective path.
  if (ctx == &app_ctx) {
    auto *metadata = http_context->GetMetadata();
    if (metadata && app_ctx.server) {
      auto *meta_container = metadata->GetModuleContextContainer();
      if (meta_container) {
        auto *meta_path = metadata->GetMetaPath();
        if (!meta_path) {
          app_ctx.logger->debug("MetaPath is null, using app-level config");
        } else {
          auto new_ctx = std::make_unique<ModuleContext>();
          new_ctx->logger = app_ctx.logger;
          new_ctx->server = app_ctx.server;

          update_module_context(*new_ctx, *app_ctx.server, meta_path,
                                *app_ctx.logger);

          // Inherit app-level tags if per-location config didn't set its own.
          if (new_ctx->application_id_tag.empty())
            new_ctx->application_id_tag = app_ctx.application_id_tag;
          if (new_ctx->remote_config_tag.empty())
            new_ctx->remote_config_tag = app_ctx.remote_config_tag;

          auto hr =
              meta_container->SetModuleContext(new_ctx.get(), g_module_id);
          if (SUCCEEDED(hr)) {
            ctx = new_ctx.release(); // container owns it now
          } else if (hr == HRESULT_FROM_WIN32(ERROR_ALREADY_ASSIGNED)) {
            // Another thread won the race — use its cached context.
            // unique_ptr auto-deletes new_ctx.
            ctx = static_cast<ModuleContext *>(
                meta_container->GetModuleContext(g_module_id));
            if (!ctx) {
              app_ctx.logger->error(
                  "Race condition: cached context disappeared");
              return RQ_NOTIFICATION_CONTINUE;
            }
          } else {
            app_ctx.logger->error(
                std::format("Failed to cache per-location context: {}",
                            std::system_category().message(hr)));
            return RQ_NOTIFICATION_CONTINUE;
          }
        }
      }
    }
  }

  if (!ctx->snippet) {
    ctx->logger->debug("Skipping RUM injection: disabled");
    return RQ_NOTIFICATION_CONTINUE;
  }

  if (!ShouldAttemptInjection(*http_response, *ctx)) {
    return RQ_NOTIFICATION_CONTINUE;
  }

  return PerformInjection(*http_context, *http_response, *ctx);
}

bool HttpModule::ShouldAttemptInjection(IHttpResponse &pHttpResponse,
                                        const ModuleContext &ctx) const {
  // First, we must validate that the content type is 'text/html'
  // Note that the value of the Content-Type header is case insensitive, and it
  // may or may not be followed by semicolon-delimited parameters
  //   ie Text/HTML;Charset="utf-8"
  // see https://www.rfc-editor.org/rfc/rfc9110#name-content-type

  if (_strnicmp(get_header(pHttpResponse, "Content-Type").data(), "text/html",
                9) != 0) {
    datadog::telemetry::counter::increment(
        telemetry::injection_skipped,
        telemetry::build_tags("reason:content-type", ctx.application_id_tag,
                              ctx.remote_config_tag));
    ctx.logger->debug("Skipping RUM injection: content type is not text/html ");
    return false;
  }

  USHORT statusCode = 0;
  pHttpResponse.GetStatus(&statusCode);

  // only inject if response code is 2xx, 4xx, 5xx
  if (statusCode < 200 || (statusCode >= 300 && statusCode < 400)) {
    ctx.logger->debug(
        std::format("Skipping RUM injection: return code {}", statusCode));
    return false;
  }

  // Check if content is compressed, skip injection if Content-Encoding header exists
  if (!get_header(pHttpResponse, "Content-Encoding").empty()) {
    datadog::telemetry::counter::increment(
        telemetry::injection_skipped,
        telemetry::build_tags("reason:compressed_html", ctx.application_id_tag,
                              ctx.remote_config_tag));
    ctx.logger->debug("Skipping RUM injection: content is compressed");
    return false;
  }

  // We must also validate that another injector in this environment hasn't
  // previously attempted injection into this response
  if (!get_header(pHttpResponse, k_injection_header).empty()) {
    datadog::telemetry::counter::increment(
        telemetry::injection_skipped,
        telemetry::build_tags("reason:already_injected", ctx.application_id_tag,
                              ctx.remote_config_tag));
    ctx.logger->debug("Skipping RUM injection: injection has "
                      "already been attempted on this response");
    return false;
  }

  return true;
}

REQUEST_NOTIFICATION_STATUS
HttpModule::PerformInjection(IHttpContext &pHttpContext,
                             IHttpResponse &pHttpResponse, ModuleContext &ctx) {
  assert(ctx.snippet != nullptr);
  assert(ctx.logger != nullptr);
  const Snippet *snippet = ctx.snippet;
  auto &logger = *ctx.logger;

  HTTP_RESPONSE *pResponseStruct = pHttpResponse.GetRawHttpResponse();
  if (!pResponseStruct) {
    logger.debug("Raw HTTP response was null");
    return RQ_NOTIFICATION_CONTINUE;
  }

  if (pResponseStruct->EntityChunkCount <= 0) {
    logger.debug("Raw HTTP response does not contain any data");
    return RQ_NOTIFICATION_CONTINUE;
  }

  Injector *injector = injector_create(snippet);
  if (!injector) {
    logger.error("Error creating injector");
    return RQ_NOTIFICATION_CONTINUE;
  }

  bool injected = false;

  // Write the existing data chunks to the injector, and then write the
  // resulting byte slices into our new response string
  for (int i = 0; i < pResponseStruct->EntityChunkCount; i++) {
    HTTP_DATA_CHUNK &dataChunk = pResponseStruct->pEntityChunks[i];
    if (dataChunk.DataChunkType != HttpDataChunkFromMemory) {
      logger.debug(
          "Skipping response which is not in memory: response type = " +
          std::to_string(dataChunk.DataChunkType));
      injector_cleanup(injector);
      return RQ_NOTIFICATION_CONTINUE;
    }

    auto chunkLength = dataChunk.FromMemory.BufferLength;

    auto injectorResult = injector_write(
        injector, (uint8_t *)dataChunk.FromMemory.pBuffer, chunkLength);
    if (injectorResult.injected) {
      const size_t bufferLength =
          sizeof(uint8_t) * (chunkLength + snippet->length);
      void *buffer = pHttpContext.AllocateRequestMemory(bufferLength);
      if (!buffer) {
        logger.error("Failed to allocate memory for injection buffer");
        injector_cleanup(injector);
        return RQ_NOTIFICATION_CONTINUE;
      }

      auto *offset = (uint8_t *)buffer;
      for (size_t i = 0; i < injectorResult.slices_length; ++i) {
        const auto &slice = injectorResult.slices[i];
        memcpy(offset, slice.start, slice.length);
        offset += slice.length;
      }

      dataChunk.FromMemory.pBuffer = (PVOID)buffer;
      dataChunk.FromMemory.BufferLength = bufferLength;

      injected = true;

      HRESULT result =
          pHttpResponse.SetHeader(k_injection_header.data(), "1", 1, TRUE);
      if (result != S_OK) {
        logger.error("Error setting injection header: " +
                     std::system_category().message(result));
        return RQ_NOTIFICATION_CONTINUE;
      }
      break;
    }
  }

  if (injected) {
    datadog::telemetry::counter::increment(
        telemetry::injection_succeed,
        telemetry::build_tags(ctx.application_id_tag, ctx.remote_config_tag));
  } else {
    auto tags = telemetry::build_tags(ctx.application_id_tag, ctx.remote_config_tag);
    if (auto content_encoding = get_header(pHttpResponse, "Content-Encoding"); !content_encoding.empty()) {
      tags.emplace_back("content_encoding:" + std::string(content_encoding));
    }

    datadog::telemetry::counter::increment(telemetry::injection_failed, tags);
  }

  injector_cleanup(injector);

  logger.debug("Writing new response");

  DWORD bytesSent;
  HRESULT result = pHttpResponse.Flush(FALSE, FALSE, &bytesSent);
  if (FAILED(result)) {
    logger.error("Error while flushing response: " +
                 std::system_category().message(result));
  }

  return RQ_NOTIFICATION_FINISH_REQUEST;
}

} // namespace datadog::rum
