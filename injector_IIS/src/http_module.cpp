// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2024-Present Datadog, Inc.

#include "http_module.h"
#include "config_reader.h"
#include "module_context.h"
#include "telemetry.h"
#include <algorithm>
#include <cassert>
#include <charconv>
#include <cstdint>
#include <datadog/telemetry/telemetry.h>
#include <format>
#include <limits>
#include <memory>
#include <string>

namespace datadog::rum {
namespace {

// Where the injector output that carried the snippet belongs in the response.
struct InjectionPoint {
  int injected_at;  // entity chunk whose write injected, or the chunk count at `injector_end`
  size_t withheld;  // bytes held back from the chunks before it
};

// Replaces the entity chunks with ones that serve `result`, the injector output that carried the
// snippet, in place of the bytes it covers.
//
// Those are the `withheld` bytes the injector held back from the chunks before `injected_at`, which
// are always a suffix of them, and the chunk at `injected_at` itself, unless the injection was only
// resolved by `injector_end` (`injected_at` is then the chunk count). The rebuilt list is: the
// chunks before, minus the withheld suffix; one chunk per slice of `result`; the chunks after,
// untouched. A slice that points into the chunk at `injected_at` is referenced where it is.
// Anything else lives in the injector, which reuses that memory on its next call, so it is copied:
// at most the head window the strategy holds back, a partial tag, and the snippet. Nothing past the
// injection point is copied, whatever the size of the response.
//
// Returns an error message, or nullptr once the response serves the rebuilt chunks.
const char* splice_injection(IHttpContext& http_context, HTTP_RESPONSE& response,
                             InjectionPoint point, const InjectorResult& result) {
  const auto [injected_at, withheld] = point;
  const int count = response.EntityChunkCount;
  const bool written = injected_at < count;
  // The injector flags the slices that point into the chunk just written: those are referenced
  // where they are. Results from `injector_end` have none.
  const auto in_place = [](const BytesSlice& slice) { return slice.from_incoming_chunk; };

  size_t preceding_length = 0;
  for (int i = 0; i < injected_at; i++) {
    preceding_length += response.pEntityChunks[i].FromMemory.BufferLength;
  }
  if (withheld > preceding_length) {
    return "Injector held back more data than it was given, skipping injection";
  }

  size_t owned = 0;
  for (size_t i = 0; i < result.slices_length; i++) {
    owned += in_place(result.slices[i]) ? 0 : result.slices[i].length;
  }
  const size_t following = written ? count - injected_at - 1 : 0;
  const size_t chunk_count = injected_at + result.slices_length + following;
  const size_t size = chunk_count * sizeof(HTTP_DATA_CHUNK) + owned;
  if (chunk_count > (std::numeric_limits<USHORT>::max)() ||
      size > (std::numeric_limits<DWORD>::max)()) {
    return "Response has too many entity chunks to rewrite for injection";
  }

  void* memory = http_context.AllocateRequestMemory(static_cast<DWORD>(size));
  if (!memory) {
    return "Failed to allocate memory for injection";
  }
  auto* chunks = static_cast<HTTP_DATA_CHUNK*>(memory);
  auto* bytes = reinterpret_cast<uint8_t*>(chunks + chunk_count);

  size_t keep = preceding_length - withheld;
  HTTP_DATA_CHUNK* next = chunks;
  for (int i = 0; i < injected_at; i++) {
    *next = response.pEntityChunks[i];
    const size_t length = (std::min)(keep, static_cast<size_t>(next->FromMemory.BufferLength));
    next->FromMemory.BufferLength = static_cast<ULONG>(length);
    keep -= length;
    next++;
  }
  for (size_t i = 0; i < result.slices_length; i++) {
    const BytesSlice& slice = result.slices[i];
    next->DataChunkType = HttpDataChunkFromMemory;
    if (in_place(slice)) {
      next->FromMemory.pBuffer = const_cast<uint8_t*>(slice.start);
    } else {
      memcpy(bytes, slice.start, slice.length);
      next->FromMemory.pBuffer = bytes;
      bytes += slice.length;
    }
    next->FromMemory.BufferLength = static_cast<ULONG>(slice.length);
    next++;
  }
  for (size_t i = 0; i < following; i++) {
    *next++ = response.pEntityChunks[injected_at + 1 + i];
  }

  response.pEntityChunks = chunks;
  response.EntityChunkCount = static_cast<USHORT>(chunk_count);
  return nullptr;
}

std::string_view get_header(const IHttpResponse& http_response, const char* header) {
  USHORT length = 0;
  PCSTR data = http_response.GetHeader(header, &length);
  if (!data) {
    return "";
  }

  return {data, length};
}

// Adds `added` bytes to the length the response announces in its Content-Length, if it announces
// one. A header that cannot be read or updated is removed instead, and the response goes out
// chunked: no length is better than a wrong one.
void extend_content_length(IHttpResponse& http_response, size_t added, Logger& logger) {
  const std::string_view announced = get_header(http_response, "Content-Length");
  if (announced.empty()) {
    return;
  }

  uint64_t length = 0;
  // Bounded by `end`: the header value is not NUL-terminated.
  const char* const begin = announced.data();
  const char* const end = begin + announced.size();
  const auto [parsed_end, error] = std::from_chars(begin, end, length);
  if (error == std::errc{} && parsed_end == end &&
      length <= (std::numeric_limits<uint64_t>::max)() - added) {
    const std::string extended = std::to_string(length + added);
    HRESULT const result = http_response.SetHeader("Content-Length", extended.c_str(),
                                                   static_cast<USHORT>(extended.size()), TRUE);
    if (SUCCEEDED(result)) {
      return;
    }
    logger.error("Error updating Content-Length: " + std::system_category().message(result));
  } else {
    logger.error("Unexpected Content-Length '" + std::string(announced) + "', removing it");
  }

  HRESULT const result = http_response.DeleteHeader("Content-Length");
  if (FAILED(result)) {
    logger.error("Error removing Content-Length: " + std::system_category().message(result));
  }
}

// What the module keeps on a request from one send to the next. Stored on the request, which frees
// it.
struct RequestState final : public IHttpStoredContext {
  // The headers went out alone, with no body, as when an application flushes them first: the
  // next send with a body starts it, though it carries no headers.
  bool awaiting_body = false;

  void CleanupStoredContext() override { delete this; }
};

// Returns the state the module keeps on the request, creating it when `create` is set, or nullptr.
RequestState* request_state(IHttpContext& http_context, bool create) {
  auto* container = http_context.GetModuleContextContainer();
  if (container == nullptr) {
    return nullptr;
  }
  if (auto* state = container->GetModuleContext(g_module_id)) {
    return static_cast<RequestState*>(state);
  }
  if (!create) {
    return nullptr;
  }
  auto state = std::unique_ptr<RequestState>(new (std::nothrow) RequestState);
  if (state == nullptr || FAILED(container->SetModuleContext(state.get(), g_module_id))) {
    return nullptr;
  }
  return state.release();  // the request owns it now
}

// Returns whether a send carries the start of the body, where the snippet goes, `headers` telling
// whether the headers go out with it. That is the first send with entity chunks: the one with the
// headers, unless they went out alone before it. The sends after it carry the rest of the body.
bool starts_body(IHttpContext& http_context, const HTTP_RESPONSE* response, bool headers) {
  const bool has_body = response != nullptr && response->EntityChunkCount > 0;
  if (headers) {
    if (!has_body) {
      // Remember it for the send that brings the body, which would otherwise be taken for a later
      // part and passed through, as every send without the headers is.
      if (auto* state = request_state(http_context, true)) {
        state->awaiting_body = true;
      }
    }
    return has_body;
  }
  RequestState* state = request_state(http_context, false);
  if (state == nullptr || !state->awaiting_body || !has_body) {
    return false;
  }
  state->awaiting_body = false;
  return true;
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

  const auto hr = meta_container->SetModuleContext(new_ctx.get(), g_module_id);
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
  const ModuleContext* ctx = resolve_cached_context(*http_context, app_ctx);

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
HttpModule::OnSendResponse(IN IHttpContext* http_context, IN ISendResponseProvider* provider) {
  assert(http_context != nullptr);
  auto* http_response = http_context->GetResponse();
  if (!http_response) {
    return RQ_NOTIFICATION_CONTINUE;
  }

  // A response can reach the module in several sends, as when ARR streams a large upstream
  // response, each one with the next part of the body. Only the one with the start of the body is
  // injected into: the later ones are passed through as they are.
  const bool headers = provider == nullptr || provider->GetHeadersBeingSent() != FALSE;
  if (!starts_body(*http_context, http_response->GetRawHttpResponse(), headers)) {
    return RQ_NOTIFICATION_CONTINUE;
  }
  const Send send{
      provider != nullptr && (provider->GetFlags() & HTTP_SEND_RESPONSE_FLAG_MORE_DATA) != 0,
      !headers};

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

  return PerformInjection(*http_context, *http_response, *ctx, send);
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

  if (const auto content_encoding = get_header(pHttpResponse, "Content-Encoding");
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
                             ModuleContext& ctx, Send send) {
  assert(ctx.snippet != nullptr);
  assert(ctx.logger != nullptr);
  const Snippet* snippet = ctx.snippet;
  auto& logger = *ctx.logger;

  // Not const: splice_injection rebuilds its entity chunk list.
  HTTP_RESPONSE* pResponseStruct = pHttpResponse.GetRawHttpResponse();
  if (!pResponseStruct) {
    logger.debug("Raw HTTP response was null");
    return RQ_NOTIFICATION_CONTINUE;
  }

  if (pResponseStruct->EntityChunkCount <= 0) {
    logger.debug("Raw HTTP response does not contain any data");
    return RQ_NOTIFICATION_CONTINUE;
  }

  // With the headers gone, the length they announced can no longer grow by the snippet.
  if (send.headers_gone && !get_header(pHttpResponse, "Content-Length").empty()) {
    logger.debug("Skipping RUM injection: length announced by headers already sent");
    return RQ_NOTIFICATION_CONTINUE;
  }

  Injector* injector = injector_create_with_strategy(snippet, ctx.injection_strategy);
  if (!injector) {
    logger.error("Error creating injector");
    return RQ_NOTIFICATION_CONTINUE;
  }

  const int count = pResponseStruct->EntityChunkCount;
  for (int i = 0; i < count; i++) {
    const HTTP_DATA_CHUNK& dataChunk = pResponseStruct->pEntityChunks[i];
    if (dataChunk.DataChunkType != HttpDataChunkFromMemory) {
      logger.debug("Skipping response which is not in memory: response type = " +
                   std::to_string(dataChunk.DataChunkType));
      injector_cleanup(injector);
      return RQ_NOTIFICATION_CONTINUE;
    }
  }

  // While it looks for the injection point, the injector holds data back: a partially matched
  // `</head>`, or for the head-open strategies up to the first kilobyte, which has to be scanned
  // for an encoding declaration before anything can go ahead of it. Past that it streams. The
  // queued chunks are left alone until the snippet comes out: they already carry every byte in
  // order, so a response without an injection point is served untouched. What the injector holds
  // back is always the end of what it was given, and it reports how much
  // (`injector_held_back_length`), which is what gets trimmed off the queued chunks when the result
  // that injects brings it back out. A response shorter than the head window resolves at
  // `injector_end`.
  bool injected = false;
  int injected_at = 0;
  size_t withheld = 0;
  InjectorResult result{};
  for (; injected_at < count; injected_at++) {
    const HTTP_DATA_CHUNK& dataChunk = pResponseStruct->pEntityChunks[injected_at];
    withheld = injector_held_back_length(injector);
    result = injector_write(injector, (uint8_t*)dataChunk.FromMemory.pBuffer,
                            dataChunk.FromMemory.BufferLength);
    if (result.injected) {
      injected = true;
      break;
    }
  }
  if (!injected) {
    withheld = injector_held_back_length(injector);
    // Without padding: the queued chunks keep the original length when nothing is injected.
    result = injector_end_without_padding(injector);
    injected = result.injected;
  }

  if (injected) {
    // Each result must be consumed before the next call to the injector, which this is.
    if (const char* error =
            splice_injection(pHttpContext, *pResponseStruct, {injected_at, withheld}, result)) {
      logger.error(error);
      injector_cleanup(injector);
      return RQ_NOTIFICATION_CONTINUE;
    }

    HRESULT const header_result = pHttpResponse.SetHeader(k_injection_header, "1", 1, TRUE);
    if (header_result != S_OK) {
      logger.error("Error setting injection header: " +
                   std::system_category().message(header_result));
      injector_cleanup(injector);
      return RQ_NOTIFICATION_CONTINUE;
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

  if (send.more_data || send.headers_gone) {
    // Part of a response sent in several sends: flushing here and finishing the request would drop
    // the sends still to come. IIS sends this part as the module left it, and the next ones as they
    // are. The headers go out with the first part, and the snippet is all the injection adds, so
    // the length the response announces grows by exactly that.
    if (injected && !send.headers_gone) {
      extend_content_length(pHttpResponse, snippet->length, logger);
    }
    return RQ_NOTIFICATION_CONTINUE;
  }

  logger.debug("Writing new response");

  DWORD bytesSent = 0;
  HRESULT const flush_result = pHttpResponse.Flush(FALSE, FALSE, &bytesSent);
  if (FAILED(flush_result)) {
    logger.error("Error while flushing response: " + std::system_category().message(flush_result));
  }

  return RQ_NOTIFICATION_FINISH_REQUEST;
}

}  // namespace datadog::rum
