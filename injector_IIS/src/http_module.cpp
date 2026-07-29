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
#include <limits>
#include <memory>
#include <string>

namespace datadog::rum {
namespace {

std::string_view get_header(IHttpResponse& http_response, const char* header) {
  USHORT length = 0;
  PCSTR data = http_response.GetHeader(header, &length);
  if (!data) {
    return "";
  }

  return {data, length};
}

// Returns the per-location context if cached on the metadata container,
// otherwise falls back to the app-level context.
ModuleContext* resolve_cached_context(IHttpContext& http_context, ModuleContext& app_ctx) {
  auto* metadata = http_context.GetMetadata();
  if (!metadata) {
    app_ctx.logger->debug("Metadata unavailable, using app-level config");
    return &app_ctx;
  }
  auto* mc = metadata->GetModuleContextContainer();
  if (!mc) {
    app_ctx.logger->debug("Metadata context container unavailable, using app-level config");
    return &app_ctx;
  }
  auto* cached = mc->GetModuleContext(g_module_id);
  if (cached) {
    return static_cast<ModuleContext*>(cached);
  }
  return &app_ctx;
}

// Reads + caches the per-location context for this request. Returns the new
// context on success, &app_ctx when no metadata path is available (use
// app-level config), or nullptr on a fatal caching failure (caller should
// short-circuit the notification).
ModuleContext* try_cache_per_location_context(IHttpContext& http_context, ModuleContext& app_ctx) {
  auto* metadata = http_context.GetMetadata();
  if (!metadata || !app_ctx.server) return &app_ctx;

  auto* meta_container = metadata->GetModuleContextContainer();
  if (!meta_container) return &app_ctx;

  const auto* meta_path = metadata->GetMetaPath();
  if (!meta_path) {
    app_ctx.logger->debug("MetaPath is null, using app-level config");
    return &app_ctx;
  }

  auto new_ctx = std::make_unique<ModuleContext>();
  new_ctx->logger = app_ctx.logger;
  new_ctx->server = app_ctx.server;

  // Cache even on Error: avoids retrying a broken COM read every request.
  // Recovery happens via OnGlobalConfigurationChange.
  update_module_context(*new_ctx, *app_ctx.server, meta_path, *app_ctx.logger);

  // Inherit app-level tags when per-location config didn't set its own.
  if (new_ctx->application_id_tag.empty()) new_ctx->application_id_tag = app_ctx.application_id_tag;
  if (new_ctx->remote_config_tag.empty()) new_ctx->remote_config_tag = app_ctx.remote_config_tag;

  auto hr = meta_container->SetModuleContext(new_ctx.get(), g_module_id);
  if (SUCCEEDED(hr)) {
    return new_ctx.release();  // container owns it now
  }
  if (hr == HRESULT_FROM_WIN32(ERROR_ALREADY_ASSIGNED)) {
    // Another thread won the race — use its cached context.
    auto* won = static_cast<ModuleContext*>(meta_container->GetModuleContext(g_module_id));
    if (!won) {
      app_ctx.logger->error("Race condition: cached context disappeared");
    }
    return won;  // nullptr on the (rare) lookup miss
  }
  app_ctx.logger->error(
      std::format("Failed to cache per-location context: {}", std::system_category().message(hr)));
  return nullptr;
}

}  // namespace

REQUEST_NOTIFICATION_STATUS
HttpModule::OnBeginRequest(IN IHttpContext* http_context, IN IHttpEventProvider* event_provider) {
  assert(http_context != nullptr);
  assert(event_provider != nullptr);
  IHttpRequest* http_request = http_context->GetRequest();
  if (!http_request) {
    return RQ_NOTIFICATION_CONTINUE;
  }

  auto* app = http_context->GetApplication();
  if (!app) {
    return RQ_NOTIFICATION_CONTINUE;
  }
  auto* app_container = app->GetModuleContextContainer();
  if (!app_container) {
    return RQ_NOTIFICATION_CONTINUE;
  }
  auto* app_opaque = app_container->GetModuleContext(g_module_id);
  if (!app_opaque) {
    return RQ_NOTIFICATION_CONTINUE;
  }

  auto& app_ctx = *static_cast<ModuleContext*>(app_opaque);

  // Best-effort per-location check: if a prior request to this location
  // already cached its context (in OnSendResponse), use it. On the first
  // request to a location, we fall back to the app-level context.
  ModuleContext* ctx = resolve_cached_context(*http_context, app_ctx);

  if (!ctx->snippet) {
    return RQ_NOTIFICATION_CONTINUE;
  }

  HRESULT const result = http_request->SetHeader(k_injection_pending_header, "1", 1, TRUE);
  if (result != S_OK) {
    event_provider->SetErrorStatus(result);
    ctx->logger->error("Error setting request header: " + std::system_category().message(result));
  }

  return RQ_NOTIFICATION_CONTINUE;
}

REQUEST_NOTIFICATION_STATUS
HttpModule::OnSendResponse(IN IHttpContext* http_context, IN ISendResponseProvider*) {
  assert(http_context != nullptr);
  auto* http_response = http_context->GetResponse();
  if (!http_response) {
    return RQ_NOTIFICATION_CONTINUE;
  }

  // Get app-level context (always needed for logger, server, tags).
  auto* app = http_context->GetApplication();
  if (!app) {
    return RQ_NOTIFICATION_CONTINUE;
  }
  auto* app_container = app->GetModuleContextContainer();
  if (!app_container) {
    return RQ_NOTIFICATION_CONTINUE;
  }
  auto* app_opaque = app_container->GetModuleContext(g_module_id);
  if (!app_opaque) {
    return RQ_NOTIFICATION_CONTINUE;
  }
  auto& app_ctx = *static_cast<ModuleContext*>(app_opaque);

  // Resolve per-<location> config. Cached on the IMetadataInfo container;
  // IIS recycles it on config change, so hot-reload works automatically.
  ModuleContext* ctx = resolve_cached_context(*http_context, app_ctx);
  if (ctx == &app_ctx) {
    ctx = try_cache_per_location_context(*http_context, app_ctx);
    if (!ctx) {
      return RQ_NOTIFICATION_CONTINUE;
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

bool HttpModule::ShouldAttemptInjection(IHttpResponse& pHttpResponse,
                                        const ModuleContext& ctx) const {
  // Match RFC 9110: Content-Type is case-insensitive and may carry parameters
  // (e.g. Text/HTML;Charset="utf-8"). IIS does not NUL-terminate the header
  // buffer, so bound _strnicmp by the explicit prefix length.
  static constexpr std::string_view kHtmlPrefix = "text/html";
  const std::string_view content_type = get_header(pHttpResponse, "Content-Type");
  if (content_type.size() < kHtmlPrefix.size() ||
      _strnicmp(content_type.data(),  // NOLINT(bugprone-suspicious-stringview-data-usage)
                kHtmlPrefix.data(), kHtmlPrefix.size()) != 0) {
    datadog::telemetry::counter::increment(
        telemetry::injection_skipped,
        telemetry::build_tags("reason:content-type", ctx.application_id_tag,
                              ctx.remote_config_tag));
    ctx.logger->debug("Skipping RUM injection: content type is not text/html ");
    return false;
  }

  USHORT statusCode = 0;
  pHttpResponse.GetStatus(&statusCode);

  // 3xx redirects have no body to inject into; <200 are informational.
  if (statusCode < 200 || (statusCode >= 300 && statusCode < 400)) {
    ctx.logger->debug(std::format("Skipping RUM injection: return code {}", statusCode));
    return false;
  }

  if (auto content_encoding = get_header(pHttpResponse, "Content-Encoding");
      !content_encoding.empty()) {
    datadog::telemetry::counter::increment(
        telemetry::injection_skipped,
        telemetry::build_tags("reason:compressed_html",
                              "content_encoding:" + std::string(content_encoding),
                              ctx.application_id_tag, ctx.remote_config_tag));
    ctx.logger->debug("Skipping RUM injection: content is compressed");
    return false;
  }

  if (!get_header(pHttpResponse, k_injection_header).empty()) {
    datadog::telemetry::counter::increment(
        telemetry::injection_skipped,
        telemetry::build_tags("reason:already_injected", ctx.application_id_tag,
                              ctx.remote_config_tag));
    ctx.logger->debug(
        "Skipping RUM injection: injection has "
        "already been attempted on this response");
    return false;
  }

  return true;
}

REQUEST_NOTIFICATION_STATUS
HttpModule::PerformInjection(IHttpContext& pHttpContext, IHttpResponse& pHttpResponse,
                             ModuleContext& ctx) {
  assert(ctx.snippet != nullptr);
  assert(ctx.logger != nullptr);
  const Snippet* snippet = ctx.snippet;
  auto& logger = *ctx.logger;

  HTTP_RESPONSE* pResponseStruct = pHttpResponse.GetRawHttpResponse();
  if (!pResponseStruct) {
    logger.debug("Raw HTTP response was null");
    return RQ_NOTIFICATION_CONTINUE;
  }

  if (pResponseStruct->EntityChunkCount <= 0) {
    logger.debug("Raw HTTP response does not contain any data");
    return RQ_NOTIFICATION_CONTINUE;
  }

  Injector* injector = injector_create_with_strategy(snippet, ctx.injection_strategy);
  if (!injector) {
    logger.error("Error creating injector");
    return RQ_NOTIFICATION_CONTINUE;
  }

  bool injected = false;

  if (ctx.injection_strategy == InjectionStrategy::HeadCloseTag) {
    // Streaming path. The `</head>` locator never holds back more than a partially matched tag, so
    // a chunk that carries the injection point is rewritten in place and the rest of the response
    // is left untouched. `injector_end` is not needed: nothing can still be buffered once the
    // snippet is out, and when it never is, the response is passed through at its original length.
    //
    // Write the existing data chunks to the injector, and then write the
    // resulting byte slices into our new response string
    for (int i = 0; i < pResponseStruct->EntityChunkCount; i++) {
      HTTP_DATA_CHUNK& dataChunk = pResponseStruct->pEntityChunks[i];
      if (dataChunk.DataChunkType != HttpDataChunkFromMemory) {
        logger.debug("Skipping response which is not in memory: response type = " +
                     std::to_string(dataChunk.DataChunkType));
        injector_cleanup(injector);
        return RQ_NOTIFICATION_CONTINUE;
      }

      auto chunkLength = dataChunk.FromMemory.BufferLength;

      auto injectorResult =
          injector_write(injector, (uint8_t*)dataChunk.FromMemory.pBuffer, chunkLength);
      if (injectorResult.injected) {
        const size_t bufferLength = sizeof(uint8_t) * (chunkLength + snippet->length);
        // See the DWORD bound in the buffering path below.
        if (bufferLength > (std::numeric_limits<DWORD>::max)()) {
          logger.error("Chunk too large to rewrite for injection: " + std::to_string(bufferLength) +
                       " bytes");
          injector_cleanup(injector);
          return RQ_NOTIFICATION_CONTINUE;
        }

        void* buffer = pHttpContext.AllocateRequestMemory(static_cast<DWORD>(bufferLength));
        if (!buffer) {
          logger.error("Failed to allocate memory for injection buffer");
          injector_cleanup(injector);
          return RQ_NOTIFICATION_CONTINUE;
        }

        auto* offset = (uint8_t*)buffer;
        for (size_t i = 0; i < injectorResult.slices_length; ++i) {
          const auto& slice = injectorResult.slices[i];
          memcpy(offset, slice.start, slice.length);
          offset += slice.length;
        }

        dataChunk.FromMemory.pBuffer = (PVOID)buffer;
        dataChunk.FromMemory.BufferLength = static_cast<ULONG>(bufferLength);

        injected = true;

        HRESULT const result = pHttpResponse.SetHeader(k_injection_header, "1", 1, TRUE);
        if (result != S_OK) {
          logger.error("Error setting injection header: " + std::system_category().message(result));
          return RQ_NOTIFICATION_CONTINUE;
        }
        break;
      }
    }
  } else {
    // Buffering path. The head-open strategies hold the first kilobyte back, so a short response
    // produces nothing at all until `injector_end`, and the injection point can land in data that
    // an earlier `injector_write` already accepted. Neither fits the in-place rewrite above, so the
    // whole response is drained into one buffer and handed back as the first chunk.
    //
    // The injector is length preserving: it emits every input byte exactly once plus either the
    // snippet or the padding that stands in for it, so the total is known up front.
    size_t total_length = 0;
    for (int i = 0; i < pResponseStruct->EntityChunkCount; i++) {
      const HTTP_DATA_CHUNK& dataChunk = pResponseStruct->pEntityChunks[i];
      if (dataChunk.DataChunkType != HttpDataChunkFromMemory) {
        logger.debug("Skipping response which is not in memory: response type = " +
                     std::to_string(dataChunk.DataChunkType));
        injector_cleanup(injector);
        return RQ_NOTIFICATION_CONTINUE;
      }
      total_length += dataChunk.FromMemory.BufferLength;
    }
    total_length += snippet->length;

    // `AllocateRequestMemory` takes a DWORD and `BufferLength` is a ULONG, while the lengths above
    // are 64 bit: a response that does not fit would be allocated short and written past its end.
    if (total_length > (std::numeric_limits<DWORD>::max)()) {
      logger.error("Response too large to buffer for injection: " + std::to_string(total_length) +
                   " bytes");
      injector_cleanup(injector);
      return RQ_NOTIFICATION_CONTINUE;
    }

    void* buffer = pHttpContext.AllocateRequestMemory(static_cast<DWORD>(total_length));
    if (!buffer) {
      logger.error("Failed to allocate memory for injection buffer");
      injector_cleanup(injector);
      return RQ_NOTIFICATION_CONTINUE;
    }

    auto* offset = (uint8_t*)buffer;
    auto* const end = (uint8_t*)buffer + total_length;
    // Each result must be consumed before the next call: consecutive calls reuse the same slice
    // storage, and a slice may point into the chunk we just handed over.
    auto append = [&](const InjectorResult& result) {
      for (size_t i = 0; i < result.slices_length; ++i) {
        const auto& slice = result.slices[i];
        if (offset + slice.length > end) {
          return false;
        }
        memcpy(offset, slice.start, slice.length);
        offset += slice.length;
      }
      injected = injected || result.injected;
      return true;
    };

    bool fits = true;
    for (int i = 0; i < pResponseStruct->EntityChunkCount && fits; i++) {
      const HTTP_DATA_CHUNK& dataChunk = pResponseStruct->pEntityChunks[i];
      fits = append(injector_write(injector, (uint8_t*)dataChunk.FromMemory.pBuffer,
                                   dataChunk.FromMemory.BufferLength));
    }
    if (fits) {
      fits = append(injector_end(injector));
    }

    if (!fits || offset != end) {
      // The length invariant is the injector's contract, so this should be unreachable. Bail out
      // and pass the response through rather than serve a truncated body.
      logger.error("Injector emitted an unexpected number of bytes, skipping injection");
      injector_cleanup(injector);
      return RQ_NOTIFICATION_CONTINUE;
    }

    pResponseStruct->pEntityChunks[0].FromMemory.pBuffer = (PVOID)buffer;
    pResponseStruct->pEntityChunks[0].FromMemory.BufferLength = (ULONG)total_length;
    for (int i = 1; i < pResponseStruct->EntityChunkCount; i++) {
      pResponseStruct->pEntityChunks[i].FromMemory.BufferLength = 0;
    }

    if (injected) {
      HRESULT const result = pHttpResponse.SetHeader(k_injection_header, "1", 1, TRUE);
      if (result != S_OK) {
        logger.error("Error setting injection header: " + std::system_category().message(result));
        injector_cleanup(injector);
        return RQ_NOTIFICATION_CONTINUE;
      }
    }
  }

  if (injected) {
    datadog::telemetry::counter::increment(
        telemetry::injection_succeed,
        telemetry::build_tags(ctx.application_id_tag, ctx.remote_config_tag));
  } else {
    datadog::telemetry::counter::increment(
        telemetry::injection_failed,
        telemetry::build_tags(ctx.application_id_tag, ctx.remote_config_tag));
  }

  injector_cleanup(injector);

  logger.debug("Writing new response");

  DWORD bytesSent = 0;
  HRESULT const result = pHttpResponse.Flush(FALSE, FALSE, &bytesSent);
  if (FAILED(result)) {
    logger.error("Error while flushing response: " + std::system_category().message(result));
  }

  return RQ_NOTIFICATION_FINISH_REQUEST;
}

}  // namespace datadog::rum
