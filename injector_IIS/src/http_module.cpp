// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2025-Present Datadog, Inc.

#include "http_module.h"
#include "config_reader.h"
#include "defer.h"
#include "module_context.h"
#include "telemetry.h"
#include <algorithm>
#include <cassert>
#include <cstdint>
#include <datadog/telemetry/telemetry.h>
#include <format>
#include <limits>
#include <memory>
#include <span>
#include <string>
#include <variant>
#include <vector>

namespace datadog::rum {
namespace {

// Where the injector output that carried the snippet belongs in the response.
struct InjectionPoint {
  int injected_at;  // entity chunk whose write injected, or the chunk count at `injector_end`
  size_t withheld;  // bytes held back from the chunks before it
};

// Has the response serve `rebuilt` as its body, through IIS's own API, in place of its entity
// chunks. IIS refusing one has the original ones written back, and returns false.
bool rewrite_body(IHttpResponse& http_response, HTTP_RESPONSE& response,
                  std::vector<HTTP_DATA_CHUNK> rebuilt) {
  std::vector<HTTP_DATA_CHUNK> original(response.pEntityChunks,
                                        response.pEntityChunks + response.EntityChunkCount);
  const auto write_body = [&](std::vector<HTTP_DATA_CHUNK>& chunks) {
    response.EntityChunkCount = 0;
    for (HTTP_DATA_CHUNK& chunk : chunks) {
      if (FAILED(http_response.WriteEntityChunkByReference(&chunk, -1))) {
        return false;
      }
    }
    return true;
  };
  if (write_body(rebuilt)) {
    return true;
  }
  write_body(original);
  return false;
}

// Rebuilds the response body around `result`, the injector output that carried the snippet, in
// place of the bytes it covers.
//
// Those are the `withheld` bytes the injector held back from the chunks before `injected_at`, which
// are always a suffix of them, and the chunk at `injected_at` itself, unless the injection was only
// resolved by `injector_end` (`injected_at` is then the chunk count). The rebuilt body is: the
// chunks before, minus the withheld suffix; one chunk per slice of `result`; the chunks after,
// untouched. A slice that points into the chunk at `injected_at` is referenced where it is.
// Anything else lives in the injector, which reuses that memory on its next call, so it is copied
// into request memory: at most the head window the injector holds back, and the snippet. Nothing
// past the injection point is copied, whatever the size of the response.
//
// The body goes through IIS's own API: the chunk descriptors are copied out, the body emptied, and
// each chunk written back by reference. IIS keeps managing its chunk array, so a write after this
// one, by IIS or another module, finds it as it expects.
//
// Returns an error message, or nullptr once the response serves the rebuilt body. Everything that
// can fail is checked before the body is emptied, except IIS refusing a chunk, after which the
// original body is written back.
const char* splice_injection(IHttpContext& http_context, IHttpResponse& http_response,
                             HTTP_RESPONSE& response, InjectionPoint point,
                             const InjectorResult& result) {
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
  // IIS takes at most 65535 chunks.
  if (injected_at + result.slices_length + following > (std::numeric_limits<USHORT>::max)() ||
      owned > (std::numeric_limits<DWORD>::max)()) {
    return "Response has too many entity chunks to rewrite for injection";
  }
  auto* bytes = static_cast<uint8_t*>(
      owned == 0 ? nullptr : http_context.AllocateRequestMemory(static_cast<DWORD>(owned)));
  if (owned != 0 && bytes == nullptr) {
    return "Failed to allocate memory for injection";
  }

  const std::vector<HTTP_DATA_CHUNK> original(response.pEntityChunks,
                                              response.pEntityChunks + count);
  std::vector<HTTP_DATA_CHUNK> rebuilt;
  rebuilt.reserve(injected_at + result.slices_length + following);
  size_t keep = preceding_length - withheld;
  for (int i = 0; i < injected_at; i++) {
    HTTP_DATA_CHUNK chunk = original[i];
    const size_t length = (std::min)(keep, static_cast<size_t>(chunk.FromMemory.BufferLength));
    chunk.FromMemory.BufferLength = static_cast<ULONG>(length);
    keep -= length;
    rebuilt.push_back(chunk);
  }
  for (size_t i = 0; i < result.slices_length; i++) {
    const BytesSlice& slice = result.slices[i];
    HTTP_DATA_CHUNK chunk{};
    chunk.DataChunkType = HttpDataChunkFromMemory;
    if (in_place(slice)) {
      chunk.FromMemory.pBuffer = const_cast<uint8_t*>(slice.start);
    } else {
      memcpy(bytes, slice.start, slice.length);
      chunk.FromMemory.pBuffer = bytes;
      bytes += slice.length;
    }
    chunk.FromMemory.BufferLength = static_cast<ULONG>(slice.length);
    rebuilt.push_back(chunk);
  }
  rebuilt.insert(rebuilt.end(), original.begin() + injected_at + (written ? 1 : 0), original.end());

  if (!rewrite_body(http_response, response, std::move(rebuilt))) {
    return "IIS refused an entity chunk, skipping injection";
  }
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

// Has the response announce the length it has once the snippet is in, if it announces one. The
// library tells what that length is; a header it cannot read, or that cannot be updated, is
// removed instead, and the response goes out chunked: no length is better than a wrong one.
// Returns whether the announced length counts the snippet.
bool extend_content_length(IHttpResponse& http_response, const Injector& injector, Logger& logger) {
  const std::string_view announced = get_header(http_response, "Content-Length");
  if (announced.empty()) {
    return false;
  }

  uint64_t length = 0;
  if (injector_extend_content_length(&injector, reinterpret_cast<const uint8_t*>(announced.data()),
                                     static_cast<uint32_t>(announced.size()), &length)) {
    const std::string extended = std::to_string(length);
    HRESULT const result = http_response.SetHeader("Content-Length", extended.c_str(),
                                                   static_cast<USHORT>(extended.size()), TRUE);
    if (SUCCEEDED(result)) {
      return true;
    }
    logger.error("Error updating Content-Length: " + std::system_category().message(result));
  } else {
    logger.error("Unexpected Content-Length '" + std::string(announced) + "', removing it");
  }

  HRESULT const result = http_response.DeleteHeader("Content-Length");
  if (FAILED(result)) {
    logger.error("Error removing Content-Length: " + std::system_category().message(result));
  }
  return false;
}

// Owns an injector, and frees it on every way out.
using InjectorPtr = std::unique_ptr<Injector, decltype(&injector_cleanup)>;

// The injector output that carried the snippet, and where it belongs in the response.
struct Injection {
  InjectionPoint point;
  InjectorResult result;
};

// Why the snippet did not go in: the counter and the reason tag it is counted with.
struct NotInjected {
  const datadog::telemetry::Counter* counter;
  const char* reason;
};

const NotInjected k_missing_header_tag{&telemetry::injection_failed, "reason:missing_header_tag"};
const NotInjected k_internal_error{&telemetry::injection_failed, "reason:internal_error"};
// A static file with `injectStaticFiles` off.
const NotInjected k_static_files_disabled{&telemetry::injection_skipped,
                                          "reason:static_files_disabled"};
// A chunk the injector cannot read: not in memory, or a file that cannot be read.
const NotInjected k_unreadable_body{&telemetry::injection_skipped, "reason:unreadable_body"};

// How much of a file the first read takes: most static pages whole. Each later one reads as much
// again as was read so far, up to k_file_read_limit bytes of files in all, while the injector has
// not decided.
constexpr uint64_t k_file_first_read = uint64_t{16} * 1024;
constexpr uint64_t k_file_read_limit = uint64_t{1024} * 1024;

// Fills `into` with the bytes at `offset` of `file`. IIS may have opened the file for
// overlapped I/O and bound it to its completion port: the read gives its own offset, so it leaves
// the file as IIS uses it, and an event whose low bit is set, so that its completion is not queued
// to that port.
bool read_file_at(HANDLE file, uint64_t offset, std::span<uint8_t> into) {
  HANDLE const event = CreateEventW(nullptr, TRUE, FALSE, nullptr);
  if (event == nullptr) {
    return false;
  }
  const auto close_event = defer([&] { CloseHandle(event); });
  OVERLAPPED overlapped{};
  overlapped.Offset = static_cast<DWORD>(offset);
  overlapped.OffsetHigh = static_cast<DWORD>(offset >> 32U);
  overlapped.hEvent = reinterpret_cast<HANDLE>(reinterpret_cast<uintptr_t>(event) | 1U);
  DWORD read = 0;
  const auto length = static_cast<DWORD>(into.size());
  if (ReadFile(file, into.data(), length, &read, &overlapped) == FALSE) {
    if (GetLastError() != ERROR_IO_PENDING ||
        WaitForSingleObject(event, INFINITE) != WAIT_OBJECT_0 ||
        GetOverlappedResult(file, &overlapped, &read, FALSE) == FALSE) {
      return false;
    }
  }
  return read == length;
}

// Has the response hold the start of the file chunk at `index` in memory, where the injector can
// read it: the chunk becomes one in memory with those bytes, followed by a file chunk with the rest
// of the range, if any is left. `read_so_far` is how much of files was read already, and grows by
// what this read takes. Returns false, the response left as it was, for a file that cannot be read,
// or once k_file_read_limit bytes were.
bool read_file_chunk(IHttpContext& http_context, IHttpResponse& http_response,
                     HTTP_RESPONSE& response, int index, uint64_t& read_so_far) {
  const HTTP_DATA_CHUNK file = response.pEntityChunks[index];
  const HANDLE handle = file.FromFileHandle.FileHandle;
  const uint64_t offset = file.FromFileHandle.ByteRange.StartingOffset.QuadPart;
  uint64_t range = file.FromFileHandle.ByteRange.Length.QuadPart;
  if (range == HTTP_BYTE_RANGE_TO_EOF) {
    LARGE_INTEGER size{};
    if (GetFileSizeEx(handle, &size) == FALSE || static_cast<uint64_t>(size.QuadPart) < offset) {
      return false;
    }
    range = static_cast<uint64_t>(size.QuadPart) - offset;
  }
  const uint64_t length =
      (std::min)({range, (std::max)(k_file_first_read, read_so_far),
                  k_file_read_limit - (std::min)(read_so_far, k_file_read_limit)});
  if (length == 0) {
    return false;
  }
  auto* bytes =
      static_cast<uint8_t*>(http_context.AllocateRequestMemory(static_cast<DWORD>(length)));
  if (bytes == nullptr || !read_file_at(handle, offset, {bytes, static_cast<size_t>(length)})) {
    return false;
  }

  std::vector<HTTP_DATA_CHUNK> rebuilt(response.pEntityChunks,
                                       response.pEntityChunks + response.EntityChunkCount);
  HTTP_DATA_CHUNK& read = rebuilt[index];
  read = {};
  read.DataChunkType = HttpDataChunkFromMemory;
  read.FromMemory.pBuffer = bytes;
  read.FromMemory.BufferLength = static_cast<ULONG>(length);
  if (length < range) {
    HTTP_DATA_CHUNK rest = file;
    rest.FromFileHandle.ByteRange.StartingOffset.QuadPart = offset + length;
    rest.FromFileHandle.ByteRange.Length.QuadPart = range - length;
    rebuilt.insert(rebuilt.begin() + index + 1, rest);
  }
  // IIS takes at most 65535 chunks.
  if (rebuilt.size() > (std::numeric_limits<USHORT>::max)() ||
      !rewrite_body(http_response, response, std::move(rebuilt))) {
    return false;
  }
  read_so_far += length;
  return true;
}

// Writes the response's entity chunks to `injector` until the snippet comes out, which happens at
// `injector_end` for a response shorter than the head window. The queued chunks are left as they
// are: they already carry every byte in order, which is how they serve a response the snippet never
// goes into. Returns why then.
//
// While it looks for the injection point, the injector holds back the first kilobyte, which decides
// where in `<head>` the snippet goes, and up to 10 KiB when the head is a run of `<meta>` and
// `<title>` tags that goes on past it. Past that it streams. What it holds back is always
// the end of what it was given, and it reports how much
// (`injector_held_back_length`), read before each call: that is what the result that injects brings
// back out, and what is trimmed off the queued chunks in its place.
//
// A file chunk, a static file served by IIS, is read into memory as the scan reaches it, a part at
// a time, up to k_file_read_limit bytes, unless `read_files` is off (`injectStaticFiles`): what the
// scan does not reach goes out from the file as it would have. A chunk that cannot be read, a file
// chunk with `read_files` off or one IIS streams for instance, ends the scan, and is
// served as it is past an injection point found before it. And when more of the response
// follows in later sends (`more_data`), the end of these chunks is not the end of the document:
// the injector is not ended, so it never injects on the strength of a document cut short.
std::variant<Injection, NotInjected> write_response(IHttpContext& http_context,
                                                    IHttpResponse& http_response,
                                                    Injector& injector, HTTP_RESPONSE& response,
                                                    bool more_data, bool read_files) {
  uint64_t file_read = 0;
  // Reading a file chunk rewrites the chunk list: the count is read on every turn.
  for (int i = 0; i < response.EntityChunkCount; i++) {
    // The injector gave up: the snippet cannot go in anymore.
    if (!injector_is_pending(&injector)) {
      return k_missing_header_tag;
    }
    if (response.pEntityChunks[i].DataChunkType == HttpDataChunkFromFileHandle) {
      if (!read_files) {
        return k_static_files_disabled;
      }
      // Read as far as allowed, and still no injection point.
      if (file_read >= k_file_read_limit) {
        return k_missing_header_tag;
      }
      if (!read_file_chunk(http_context, http_response, response, i, file_read)) {
        return k_unreadable_body;
      }
    }
    const HTTP_DATA_CHUNK& chunk = response.pEntityChunks[i];
    if (chunk.DataChunkType != HttpDataChunkFromMemory) {
      return k_unreadable_body;
    }
    const size_t withheld = injector_held_back_length(&injector);
    const InjectorResult result =
        injector_write(&injector, static_cast<const uint8_t*>(chunk.FromMemory.pBuffer),
                       chunk.FromMemory.BufferLength);
    if (result.injected) {
      return Injection{{i, withheld}, result};
    }
  }
  if (more_data) {
    return k_missing_header_tag;
  }
  const size_t withheld = injector_held_back_length(&injector);
  // Without padding: the queued chunks keep the original length when nothing is injected.
  const InjectorResult result = injector_end_without_padding(&injector);
  if (result.injected) {
    return Injection{{response.EntityChunkCount, withheld}, result};
  }
  return k_missing_header_tag;
}

// Has the response serve `injection`: its entity chunks rebuilt around the snippet, then its
// headers, unless they are gone already, saying the snippet is in and announcing the length it now
// has. Must be called before the next call to the injector, which reuses the memory the result
// points to.
//
// Returns an error message, or nullptr. Splicing only fails before it touches the response, which
// is then served untouched.
const char* apply_injection(IHttpContext& http_context, IHttpResponse& http_response,
                            HTTP_RESPONSE& response, const Injection& injection,
                            const Injector& injector, bool headers_gone, Logger& logger) {
  if (const char* error = splice_injection(http_context, http_response, response, injection.point,
                                           injection.result)) {
    return error;
  }
  if (headers_gone) {
    // Sent already, and announcing no length: the caller checks that.
    return nullptr;
  }
  extend_content_length(http_response, injector, logger);
  if (FAILED(http_response.SetHeader(k_injection_header, "1", 1, TRUE))) {
    return "Error setting the injection header";
  }
  return nullptr;
}

// Owns a snippet.
using SnippetPtr = std::unique_ptr<Snippet, decltype(&snippet_cleanup)>;

// What the module keeps on a request from one notification to the next. Stored on the request,
// which frees it.
struct RequestState final : public IHttpStoredContext {
  // The request came with the injection-pending header: an injector downstream, a proxy in front
  // of this server, injects the response unless it says it is injected already. Only the first
  // part of a response can say so, its headers going out with it, so a response is then never
  // injected in a later part.
  bool downstream_injector = false;

  // The headers went out alone, with no body, as when an application flushes them first: the
  // next send with a body starts it, though it carries no headers.
  bool awaiting_body = false;

  // The injection carried over the parts of a response, while the snippet may still go into a
  // later one. The injector borrows `snippet`, a copy of the configured one that a configuration
  // reload cannot free; declared after it, it is destroyed first.
  SnippetPtr snippet{nullptr, snippet_cleanup};
  InjectorPtr injector{nullptr, injector_cleanup};
  // Whether the announced length counts the snippet already: the last part is then padded to it
  // if the snippet never goes in.
  bool padded = false;
  size_t snippet_length = 0;

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
      // part, as every send without the headers is.
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

// Appends `length` spaces to the response, standing in for a snippet its announced length counts
// but that never went in. Returns an error message, or nullptr.
const char* append_padding(IHttpContext& http_context, IHttpResponse& http_response,
                           HTTP_RESPONSE& response, size_t length) {
  if (length > (std::numeric_limits<uint32_t>::max)()) {
    return "Padding too large";
  }
  // The splice copies the slices it does not reference in place, so the spaces can be local.
  const std::vector<uint8_t> spaces(length, ' ');
  const BytesSlice slice{static_cast<uint32_t>(length), spaces.data(), false};
  const InjectorResult padding{1, &slice, false};
  return splice_injection(http_context, http_response, response, {response.EntityChunkCount, 0},
                          padding);
}

// Returns whether every entity chunk of the response is in memory, where the injector can read it.
bool all_in_memory(const HTTP_RESPONSE& response) {
  for (int i = 0; i < response.EntityChunkCount; i++) {
    if (response.pEntityChunks[i].DataChunkType != HttpDataChunkFromMemory) {
      return false;
    }
  }
  return true;
}

// Leaves the last `held` bytes out of the response, what the injector holds back of the head window
// it has yet to decide on: they go out with a later part, which the injector hands them back with.
// Returns an error message, or nullptr. Fails before it touches the response.
const char* hold_back(IHttpResponse& http_response, HTTP_RESPONSE& response, size_t held) {
  std::vector<HTTP_DATA_CHUNK> kept(response.pEntityChunks,
                                    response.pEntityChunks + response.EntityChunkCount);
  while (held != 0 && !kept.empty()) {
    auto& length = kept.back().FromMemory.BufferLength;
    const size_t trimmed = (std::min)(held, static_cast<size_t>(length));
    length -= static_cast<ULONG>(trimmed);
    held -= trimmed;
    if (length == 0) {
      kept.pop_back();
    }
  }
  if (held != 0) {
    return "Injector held back more data than it was given";
  }
  if (!rewrite_body(http_response, response, std::move(kept))) {
    return "IIS refused an entity chunk, not carrying the injection over";
  }
  return nullptr;
}

// What became of a part written to a carried injector.
struct Streamed {
  const char* error;      // nullptr, or the response is left as it was
  bool injected;          // the snippet went into this part
  bool pending;           // the injector may still inject into a later part
  bool ended;             // the injector ended the document, padded as asked if it did not inject
  bool readable = false;  // every chunk of this part was in memory
};

// Rebuilds a part after the first out of what the carried `injector` hands back for it: the bytes
// it held back from earlier parts, this part's own, and the snippet if it goes in, in order. What
// it still holds back at the end goes out with a later part. `end` ends the document with this
// part, padded to the announced length (`padded`) if the snippet never went in.
//
// A chunk not in memory, which the injector cannot read, ends the scan: what it holds back goes out
// ahead of that chunk, which goes out as it is with the ones after it. The injector is not ended
// then, even with the document: where the head closes lies in bytes it never read, and the snippet
// would go after them, out of the head. Padding the response is left to the caller.
Streamed stream_part(IHttpContext& http_context, IHttpResponse& http_response,
                     HTTP_RESPONSE& response, Injector& injector, bool end, bool padded) {
  std::vector<HTTP_DATA_CHUNK> rebuilt;
  bool injected = false;
  // Slices that point into the chunk just written are referenced where they are; the others live
  // in the injector, which reuses that memory on its next call, so they are copied out right away.
  const auto append = [&](const InjectorResult& result) {
    injected = injected || result.injected;
    for (size_t i = 0; i < result.slices_length; i++) {
      const BytesSlice& slice = result.slices[i];
      if (slice.length == 0) {
        continue;
      }
      HTTP_DATA_CHUNK chunk{};
      chunk.DataChunkType = HttpDataChunkFromMemory;
      chunk.FromMemory.BufferLength = slice.length;
      if (slice.from_incoming_chunk) {
        chunk.FromMemory.pBuffer = const_cast<uint8_t*>(slice.start);
      } else {
        void* copy = http_context.AllocateRequestMemory(slice.length);
        if (copy == nullptr) {
          return false;
        }
        memcpy(copy, slice.start, slice.length);
        chunk.FromMemory.pBuffer = copy;
      }
      rebuilt.push_back(chunk);
    }
    return true;
  };

  const int count = response.EntityChunkCount;
  bool readable = true;
  for (int i = 0; i < count && readable; i++) {
    const HTTP_DATA_CHUNK& chunk = response.pEntityChunks[i];
    readable = chunk.DataChunkType == HttpDataChunkFromMemory;
    const bool appended =
        readable ? append(injector_write(&injector,
                                         static_cast<const uint8_t*>(chunk.FromMemory.pBuffer),
                                         chunk.FromMemory.BufferLength))
                 : append(injector_flush(&injector));
    if (!appended) {
      return {"Failed to allocate memory for injection", false, false, false};
    }
    if (!readable) {
      rebuilt.insert(rebuilt.end(), response.pEntityChunks + i, response.pEntityChunks + count);
    }
  }
  const bool ended = end && readable;
  if (ended &&
      !append(padded ? injector_end(&injector) : injector_end_without_padding(&injector))) {
    return {"Failed to allocate memory for injection", false, false, false};
  }
  // IIS takes at most 65535 chunks.
  if (rebuilt.size() > (std::numeric_limits<USHORT>::max)()) {
    return {"Response has too many entity chunks to rewrite for injection", false, false, false};
  }
  if (!rewrite_body(http_response, response, std::move(rebuilt))) {
    return {"IIS refused an entity chunk, skipping injection", false, false, false};
  }
  return {nullptr, injected, readable && !end && injector_is_pending(&injector), ended, readable};
}

// Counts an injection that succeeded, for telemetry.
void count_injected(const ModuleContext& ctx) {
  datadog::telemetry::counter::increment(
      telemetry::injection_succeed,
      telemetry::build_tags(ctx.application_id_tag, ctx.remote_config_tag));
}

// Counts a response the snippet did not go into, with why, for telemetry.
void count_injection(const ModuleContext& ctx, NotInjected why) {
  datadog::telemetry::counter::increment(
      *why.counter,
      telemetry::build_tags(why.reason, ctx.application_id_tag, ctx.remote_config_tag));
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

// Returns the context of the request's application, or of its `<location>` when it has its own
// configuration, or nullptr if there is none.
ModuleContext* resolve_module_context(IHttpContext& http_context) {
  // Get app-level context (always needed for logger, server, tags).
  auto* app = http_context.GetApplication();
  if (!app) {
    return nullptr;
  }
  auto* app_container = app->GetModuleContextContainer();
  if (!app_container) {
    return nullptr;
  }
  auto* app_opaque = app_container->GetModuleContext(g_module_id);
  if (!app_opaque) {
    return nullptr;
  }
  auto& app_ctx = *static_cast<ModuleContext*>(app_opaque);

  // Resolve per-<location> config. Cached on the IMetadataInfo container;
  // IIS recycles it on config change, so hot-reload works automatically.
  ModuleContext* ctx = resolve_cached_context(http_context, app_ctx);
  if (ctx == &app_ctx) {
    ctx = try_cache_per_location_context(http_context, app_ctx);
  }
  return ctx;
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

  // The same configuration the response is injected with: RUM may be on for this location only.
  const ModuleContext* ctx = resolve_module_context(*http_context);
  if (ctx == nullptr) {
    return RQ_NOTIFICATION_CONTINUE;
  }

  if (!ctx->snippet) {
    // RUM disabled: nothing else is done for the request.
    return RQ_NOTIFICATION_CONTINUE;
  }

  // Before this module sets it for the server it forwards to: a request that already has it comes
  // from an injector downstream. Only looked at with RUM on, so that a request costs nothing more
  // with it off.
  USHORT pending_length = 0;
  if (http_request->GetHeader(k_injection_pending_header, &pending_length) != nullptr &&
      pending_length != 0) {
    if (auto* state = request_state(*http_context, true)) {
      state->downstream_injector = true;
    }
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
  // injected into; the later ones go to the injector it left, if any.
  const bool headers = provider == nullptr || provider->GetHeadersBeingSent() != FALSE;
  const Send send{
      provider != nullptr && (provider->GetFlags() & HTTP_SEND_RESPONSE_FLAG_MORE_DATA) != 0,
      !headers};
  if (!headers) {
    // A send without the headers continues a response, and only one the module took on has
    // anything left to do: any other, every one with RUM disabled among them, ends at this lookup.
    if (request_state(*http_context, false) == nullptr) {
      return RQ_NOTIFICATION_CONTINUE;
    }
    if (!starts_body(*http_context, http_response->GetRawHttpResponse(), headers)) {
      return ContinueInjection(*http_context, *http_response, send.more_data);
    }
  }

  ModuleContext* ctx = resolve_module_context(*http_context);
  if (!ctx) {
    return RQ_NOTIFICATION_CONTINUE;
  }

  if (!ctx->snippet) {
    // RUM disabled: nothing else is done for the response, not even a debug line, whose message
    // alone would cost an allocation every request.
    return RQ_NOTIFICATION_CONTINUE;
  }

  // The headers going out alone, ahead of the body: noted for the send that starts it.
  if (headers && !starts_body(*http_context, http_response->GetRawHttpResponse(), headers)) {
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

  // A 206 carries a range of the document, which the snippet would corrupt.
  if (statusCode == 206) {
    count_injection(ctx, {&telemetry::injection_skipped, "reason:partial_content"});
    ctx.logger->debug("Skipping RUM injection: partial content");
    return false;
  }

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

  // The first of several parts may leave an injector to carry over to the next ones, which has to
  // outlive the configured snippet: it is created from a copy of it then.
  const RequestState* state = request_state(pHttpContext, false);
  const bool may_carry = send.more_data && (state == nullptr || !state->downstream_injector);
  SnippetPtr snippet(may_carry ? snippet_copy(ctx.snippet) : nullptr, snippet_cleanup);
  InjectorPtr injector(
      injector_create_with_strategy(snippet ? snippet.get() : ctx.snippet, ctx.injection_strategy),
      injector_cleanup);
  if (!injector) {
    logger.error("Error creating injector");
    count_injection(ctx, k_internal_error);
    return RQ_NOTIFICATION_CONTINUE;
  }

  const auto written = write_response(pHttpContext, pHttpResponse, *injector, *pResponseStruct,
                                      send.more_data, ctx.inject_static_files);
  if (const auto* injection = std::get_if<Injection>(&written)) {
    if (const char* error = apply_injection(pHttpContext, pHttpResponse, *pResponseStruct,
                                            *injection, *injector, send.headers_gone, logger)) {
      logger.error(error);
      count_injection(ctx, k_internal_error);
      return RQ_NOTIFICATION_CONTINUE;
    }
    count_injected(ctx);
  } else if (may_carry && all_in_memory(*pResponseStruct) && injector_is_pending(injector.get())) {
    // This part goes out, but for what the injector holds back of a head window it has yet to
    // decide on, and the next ones go to the same injector.
    auto* carried = snippet == nullptr ? nullptr : request_state(pHttpContext, true);
    const char* error = carried == nullptr ? "Error storing the injection for the next parts"
                                           : hold_back(pHttpResponse, *pResponseStruct,
                                                       injector_held_back_length(injector.get()));
    if (error != nullptr) {
      logger.error(error);
      count_injection(ctx, k_internal_error);
    } else {
      // The headers go out with this part, if they have not already, before the snippet goes in,
      // if it ever does: the announced length counts it already, and the last part is padded to
      // it otherwise. Gone already, they announce no length.
      carried->padded =
          !send.headers_gone && extend_content_length(pHttpResponse, *injector, logger);
      carried->snippet_length = snippet->length;
      carried->snippet = std::move(snippet);
      carried->injector = std::move(injector);
      return RQ_NOTIFICATION_CONTINUE;
    }
  } else {
    count_injection(ctx, std::get<NotInjected>(written));
  }

  if (send.more_data || send.headers_gone) {
    // Part of a response sent in several sends: flushing here and finishing the request would drop
    // the sends still to come. IIS sends this part as the module left it, and the next ones as they
    // are.
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

REQUEST_NOTIFICATION_STATUS
HttpModule::ContinueInjection(IHttpContext& pHttpContext, IHttpResponse& pHttpResponse,
                              bool more_data) {
  RequestState* state = request_state(pHttpContext, false);
  if (state == nullptr || (state->injector == nullptr && !state->padded)) {
    return RQ_NOTIFICATION_CONTINUE;
  }
  HTTP_RESPONSE* pResponseStruct = pHttpResponse.GetRawHttpResponse();
  if (pResponseStruct == nullptr) {
    return RQ_NOTIFICATION_CONTINUE;
  }
  // For logging and telemetry only: the injector owns what it injects.
  const ModuleContext* ctx = resolve_module_context(pHttpContext);
  const auto log_error = [ctx](const char* error) {
    if (ctx != nullptr) {
      ctx->logger->error(error);
    }
  };
  const auto resolve = [&](const Streamed& streamed) {
    if (ctx != nullptr) {
      if (streamed.injected) {
        count_injected(*ctx);
      } else {
        count_injection(*ctx, streamed.error != nullptr ? k_internal_error
                              : streamed.readable       ? k_missing_header_tag
                                                        : k_unreadable_body);
      }
    }
    state->injector.reset();
    state->snippet.reset();
  };

  if (state->injector != nullptr) {
    const Streamed streamed = stream_part(pHttpContext, pHttpResponse, *pResponseStruct,
                                          *state->injector, !more_data, state->padded);
    if (streamed.error != nullptr) {
      // What the injector held back from earlier parts is lost with it: a failed allocation or
      // write, after which the response is unlikely to go out whole anyway.
      log_error(streamed.error);
    } else if (streamed.injected || streamed.ended) {
      // In, or padded by the end of the document in its place.
      state->padded = false;
    }
    if (streamed.error != nullptr || !streamed.pending) {
      resolve(streamed);
    }
  }

  if (more_data) {
    return RQ_NOTIFICATION_CONTINUE;
  }

  // The last part, past an injector given up earlier: padded to the length announced.
  if (state->padded) {
    state->padded = false;
    if (const char* error =
            append_padding(pHttpContext, pHttpResponse, *pResponseStruct, state->snippet_length)) {
      log_error(error);
    }
  }
  return RQ_NOTIFICATION_CONTINUE;
}

}  // namespace datadog::rum
