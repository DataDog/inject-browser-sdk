// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2024-Present Datadog, Inc.

// Integration-style test that exercises the full IIS module pipeline:
//   RegisterModule → OnGlobalApplicationStart (config reading)
//     → OnBeginRequest (pending header) / OnSendResponse (injection into HTML)
//
// Uses mock IAppHost* COM interfaces to feed configuration, and a mock
// HTTP response containing an HTML body with <head></head>.

#include "pipeline_mocks.h"

using namespace datadog::rum;

// Same, with the body split across one entity chunk per element of `bodies`.
static void setupChunkedHtmlResponse(MockHttpContext& ctx, MockHttpResponse& resp,
                                     IHttpApplication* app, MockMetadataInfo* metadata,
                                     PreparedChunkedResponse& pr,
                                     const std::vector<std::string>& bodies) {
  setupHtmlResponseMetadata(ctx, resp, app, metadata);

  pr.bodies = bodies;
  pr.chunks.clear();
  for (auto& body : pr.bodies) {
    HTTP_DATA_CHUNK chunk{};
    chunk.DataChunkType = HttpDataChunkFromMemory;
    chunk.FromMemory.pBuffer = (PVOID)body.data();
    chunk.FromMemory.BufferLength = (ULONG)body.size();
    pr.chunks.push_back(chunk);
  }
  pr.raw = {};
  pr.raw.EntityChunkCount = (USHORT)pr.chunks.size();
  pr.raw.pEntityChunks = pr.chunks.data();

  EXPECT_CALL(resp, GetRawHttpResponse()).WillRepeatedly(Return(&pr.raw));
}

// The body IIS would send: every entity chunk at whatever length the module left it at.
static std::string assembleResponseBody(const PreparedChunkedResponse& pr) {
  std::string body;
  for (const auto& chunk : pr.chunks) {
    body.append(static_cast<const char*>(chunk.FromMemory.pBuffer), chunk.FromMemory.BufferLength);
  }
  return body;
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

TEST(InjectionPipeline, FullModuleLifecycle) {
  MockRumConfigElement rumConfig;
  MockAdminManager adminManager(&rumConfig);
  MockServer mockServer;
  EXPECT_CALL(mockServer, GetAdminManager()).WillRepeatedly(Return(&adminManager));

  auto mod = registerModule(mockServer);
  ASSERT_NE(mod.global, nullptr);
  ASSERT_NE(mod.factory, nullptr);

  // App start → config reading
  MockAppWithContext app;
  MockAppStartProvider appStart(app);
  EXPECT_EQ(mod.global->OnGlobalApplicationStart(&appStart), GL_NOTIFICATION_CONTINUE);

  auto* ctx =
      static_cast<ModuleContext*>(app.GetModuleContextContainer()->GetModuleContext(nullptr));
  ASSERT_NE(ctx, nullptr);
  ASSERT_NE(ctx->snippet, nullptr);

  // HTTP module
  CHttpModule* httpMod = nullptr;
  ASSERT_EQ(mod.factory->GetHttpModule(&httpMod, nullptr), S_OK);

  // Simulate response
  MockHttpContext httpCtx;
  MockHttpResponse httpResp;
  MockMetadataInfo metadata(L"MACHINE/WEBROOT/APPHOST/Default Web Site");
  PreparedResponse pr;
  const char* html =
      "<!DOCTYPE html><html><head><title>Test</title></head>"
      "<body>Hello</body></html>";
  setupHtmlResponse(httpCtx, httpResp, &app, &metadata, pr, html);

  EXPECT_CALL(httpResp, SetHeader(Matcher<PCSTR>(StrEq(k_injection_header)), _, _, _))
      .WillOnce(Return(S_OK));
  EXPECT_CALL(httpResp, Flush(_, _, _, _)).WillOnce(Return(S_OK));

  EXPECT_EQ(httpMod->OnSendResponse(&httpCtx, nullptr), RQ_NOTIFICATION_FINISH_REQUEST);

  const std::string output(static_cast<const char*>(pr.chunk.FromMemory.pBuffer),
                           pr.chunk.FromMemory.BufferLength);
  EXPECT_NE(output.find("DD_RUM"), std::string::npos) << "Expected DD_RUM in output, got:\n"
                                                      << output;
  EXPECT_NE(output.find("test-app-id"), std::string::npos);
  EXPECT_NE(output.find("test-client-token"), std::string::npos);
  EXPECT_NE(output.find("test-service"), std::string::npos);
  EXPECT_NE(output.find("</head>"), std::string::npos);

  mod.factory->Terminate();
  mod.global->Terminate();
}

// The `injectionStrategy` attribute reaches the injector. Without it the module would be stuck on
// the `</head>` strategy and the head-open strategies would be unreachable from IIS.
TEST(InjectionPipeline, InjectionStrategyFromConfig) {
  struct TestCase {
    std::string name;
    InjectionStrategy strategy;
    std::string expected_before_snippet;
  };

  const std::string html = "<html><head><meta charset='utf-8'></head><body>hi</body></html>";

  const std::vector<TestCase> cases = {
      // The default: the snippet lands right before `</head>`.
      {"head close tag", InjectionStrategy::HeadCloseTag, "<html><head><meta charset='utf-8'>"},
      // Injects right after the encoding declaration instead. Reaching this at all needs the
      // buffering path in PerformInjection, since a short response emits nothing until
      // `injector_end`.
      {"head open tag", InjectionStrategy::HeadOpenTagWithCloseTagFallback,
       "<html><head><meta charset='utf-8'>"},
  };

  for (const auto& test_case : cases) {
    SCOPED_TRACE(test_case.name);

    MockRumConfigElement rumConfig;
    rumConfig.SetInjectionStrategy(test_case.strategy);
    MockAdminManager adminManager(&rumConfig);
    MockServer mockServer;
    EXPECT_CALL(mockServer, GetAdminManager()).WillRepeatedly(Return(&adminManager));

    auto mod = registerModule(mockServer);
    ASSERT_NE(mod.global, nullptr);

    MockAppWithContext app;
    MockAppStartProvider appStart(app);
    ASSERT_EQ(mod.global->OnGlobalApplicationStart(&appStart), GL_NOTIFICATION_CONTINUE);

    auto* ctx =
        static_cast<ModuleContext*>(app.GetModuleContextContainer()->GetModuleContext(nullptr));
    ASSERT_NE(ctx, nullptr);
    ASSERT_NE(ctx->snippet, nullptr);
    EXPECT_EQ(ctx->injection_strategy, test_case.strategy);

    CHttpModule* httpMod = nullptr;
    ASSERT_EQ(mod.factory->GetHttpModule(&httpMod, nullptr), S_OK);

    MockHttpContext httpCtx;
    MockHttpResponse httpResp;
    MockMetadataInfo metadata(L"MACHINE/WEBROOT/APPHOST/Default Web Site");
    PreparedResponse pr;
    setupHtmlResponse(httpCtx, httpResp, &app, &metadata, pr, html.c_str());

    EXPECT_CALL(httpResp, SetHeader(Matcher<PCSTR>(StrEq(k_injection_header)), _, _, _))
        .WillOnce(Return(S_OK));
    EXPECT_CALL(httpResp, Flush(_, _, _, _)).WillOnce(Return(S_OK));

    EXPECT_EQ(httpMod->OnSendResponse(&httpCtx, nullptr), RQ_NOTIFICATION_FINISH_REQUEST);

    const std::string output(static_cast<const char*>(pr.chunk.FromMemory.pBuffer),
                             pr.chunk.FromMemory.BufferLength);
    ASSERT_NE(output.find("DD_RUM"), std::string::npos) << "no snippet in output:\n" << output;
    EXPECT_EQ(output.substr(0, test_case.expected_before_snippet.size()),
              test_case.expected_before_snippet)
        << "full output:\n"
        << output;
    EXPECT_NE(output.find("<body>hi</body></html>"), std::string::npos) << output;

    mod.factory->Terminate();
    mod.global->Terminate();
  }
}

// A `</head>` straddling entity chunks. The locator holds the partial tag back and re-emits it with
// the chunk that completes it, so the rewritten chunk has to be sized for those held-back bytes,
// and the chunk that carried them has to give them up: sizing the allocation from this chunk plus
// the snippet writes past its end, and leaving the earlier chunk alone serves the partial tag
// twice.
TEST(InjectionPipeline, HeadCloseTagSplitAcrossEntityChunks) {
  const std::string html =
      "<!DOCTYPE html><html><head><title>t</title></head><body>hi</body></html>";
  const std::string close_tag = "</head>";
  const size_t tag_start = html.find(close_tag);
  ASSERT_NE(tag_start, std::string::npos);

  // Every split point within the tag, plus the ones just before and just after it.
  for (size_t split = tag_start; split <= tag_start + close_tag.size(); split++) {
    SCOPED_TRACE("chunk boundary at byte " + std::to_string(split));

    MockRumConfigElement rumConfig;
    MockAdminManager adminManager(&rumConfig);
    MockServer mockServer;
    EXPECT_CALL(mockServer, GetAdminManager()).WillRepeatedly(Return(&adminManager));

    auto mod = registerModule(mockServer);
    ASSERT_NE(mod.global, nullptr);

    MockAppWithContext app;
    MockAppStartProvider appStart(app);
    ASSERT_EQ(mod.global->OnGlobalApplicationStart(&appStart), GL_NOTIFICATION_CONTINUE);

    auto* ctx =
        static_cast<ModuleContext*>(app.GetModuleContextContainer()->GetModuleContext(nullptr));
    ASSERT_NE(ctx, nullptr);
    ASSERT_NE(ctx->snippet, nullptr);
    ASSERT_EQ(ctx->injection_strategy, InjectionStrategy::HeadCloseTag);

    CHttpModule* httpMod = nullptr;
    ASSERT_EQ(mod.factory->GetHttpModule(&httpMod, nullptr), S_OK);

    MockHttpContext httpCtx;
    MockHttpResponse httpResp;
    MockMetadataInfo metadata(L"MACHINE/WEBROOT/APPHOST/Default Web Site");
    PreparedChunkedResponse pr;
    setupChunkedHtmlResponse(httpCtx, httpResp, &app, &metadata, pr,
                             {html.substr(0, split), html.substr(split)});

    EXPECT_CALL(httpResp, SetHeader(Matcher<PCSTR>(StrEq(k_injection_header)), _, _, _))
        .WillOnce(Return(S_OK));
    EXPECT_CALL(httpResp, Flush(_, _, _, _)).WillOnce(Return(S_OK));

    EXPECT_EQ(httpMod->OnSendResponse(&httpCtx, nullptr), RQ_NOTIFICATION_FINISH_REQUEST);
    EXPECT_TRUE(httpCtx.AllocationGuardIntact()) << "wrote past the end of the injection buffer";

    // The snippet goes right before `</head>` and nothing else moves: no byte of the response is
    // dropped, duplicated or reordered around it.
    const std::string body = assembleResponseBody(pr);
    ASSERT_EQ(body.size(), html.size() + ctx->snippet->length) << "body:\n" << body;
    EXPECT_EQ(body.substr(0, tag_start), html.substr(0, tag_start)) << "body:\n" << body;
    EXPECT_EQ(body.substr(body.size() - (html.size() - tag_start)), html.substr(tag_start))
        << "body:\n"
        << body;
    EXPECT_NE(body.find("DD_RUM"), std::string::npos) << "no snippet in body:\n" << body;

    mod.factory->Terminate();
    mod.global->Terminate();
  }
}

// A partial `</head>` at the very end of the response never resolves into a tag. Nothing is
// injected and nothing is rewritten, so the held-back bytes have to reach the client as part of the
// chunk that already carries them.
TEST(InjectionPipeline, UnresolvedHeadCloseTagLeavesTheResponseUntouched) {
  const std::vector<std::string> bodies = {"<html><body>hi</body>", "</he"};

  MockRumConfigElement rumConfig;
  MockAdminManager adminManager(&rumConfig);
  MockServer mockServer;
  EXPECT_CALL(mockServer, GetAdminManager()).WillRepeatedly(Return(&adminManager));

  auto mod = registerModule(mockServer);
  ASSERT_NE(mod.global, nullptr);

  MockAppWithContext app;
  MockAppStartProvider appStart(app);
  ASSERT_EQ(mod.global->OnGlobalApplicationStart(&appStart), GL_NOTIFICATION_CONTINUE);

  CHttpModule* httpMod = nullptr;
  ASSERT_EQ(mod.factory->GetHttpModule(&httpMod, nullptr), S_OK);

  MockHttpContext httpCtx;
  MockHttpResponse httpResp;
  MockMetadataInfo metadata(L"MACHINE/WEBROOT/APPHOST/Default Web Site");
  PreparedChunkedResponse pr;
  setupChunkedHtmlResponse(httpCtx, httpResp, &app, &metadata, pr, bodies);

  EXPECT_CALL(httpResp, SetHeader(Matcher<PCSTR>(StrEq(k_injection_header)), _, _, _)).Times(0);
  EXPECT_CALL(httpResp, Flush(_, _, _, _)).WillOnce(Return(S_OK));

  EXPECT_EQ(httpMod->OnSendResponse(&httpCtx, nullptr), RQ_NOTIFICATION_FINISH_REQUEST);
  EXPECT_EQ(assembleResponseBody(pr), "<html><body>hi</body></he");

  mod.factory->Terminate();
  mod.global->Terminate();
}

// A response with no injection point at all leaves the injector padding the body with spaces to
// keep its length constant. The buffering path must not serve that padding: like the streaming
// path, it leaves the queued chunks as they are when nothing was injected.
TEST(InjectionPipeline, UninjectedBufferedResponseLeavesTheResponseUntouched) {
  const std::vector<std::string> bodies = {"<body><pre>hel", "lo"};

  MockRumConfigElement rumConfig;
  rumConfig.SetInjectionStrategy(InjectionStrategy::HeadOpenTagWithCloseTagFallback);
  MockAdminManager adminManager(&rumConfig);
  MockServer mockServer;
  EXPECT_CALL(mockServer, GetAdminManager()).WillRepeatedly(Return(&adminManager));

  auto mod = registerModule(mockServer);
  ASSERT_NE(mod.global, nullptr);

  MockAppWithContext app;
  MockAppStartProvider appStart(app);
  ASSERT_EQ(mod.global->OnGlobalApplicationStart(&appStart), GL_NOTIFICATION_CONTINUE);

  CHttpModule* httpMod = nullptr;
  ASSERT_EQ(mod.factory->GetHttpModule(&httpMod, nullptr), S_OK);

  MockHttpContext httpCtx;
  MockHttpResponse httpResp;
  MockMetadataInfo metadata(L"MACHINE/WEBROOT/APPHOST/Default Web Site");
  PreparedChunkedResponse pr;
  setupChunkedHtmlResponse(httpCtx, httpResp, &app, &metadata, pr, bodies);

  EXPECT_CALL(httpResp, SetHeader(Matcher<PCSTR>(StrEq(k_injection_header)), _, _, _)).Times(0);
  EXPECT_CALL(httpResp, Flush(_, _, _, _)).WillOnce(Return(S_OK));

  EXPECT_EQ(httpMod->OnSendResponse(&httpCtx, nullptr), RQ_NOTIFICATION_FINISH_REQUEST);
  EXPECT_TRUE(httpCtx.AllocationGuardIntact()) << "wrote past the end of the injection buffer";
  EXPECT_EQ(assembleResponseBody(pr), "<body><pre>hello");

  mod.factory->Terminate();
  mod.global->Terminate();
}

// The head-open strategies buffer the whole body, so a response too large to copy is passed through
// uninstrumented rather than doubling the worker process' memory for it.
TEST(InjectionPipeline, OversizedResponseIsNotBuffered) {
  MockRumConfigElement rumConfig;
  rumConfig.SetInjectionStrategy(InjectionStrategy::HeadOpenTagWithCloseTagFallback);
  MockAdminManager adminManager(&rumConfig);
  MockServer mockServer;
  EXPECT_CALL(mockServer, GetAdminManager()).WillRepeatedly(Return(&adminManager));

  auto mod = registerModule(mockServer);
  ASSERT_NE(mod.global, nullptr);

  MockAppWithContext app;
  MockAppStartProvider appStart(app);
  ASSERT_EQ(mod.global->OnGlobalApplicationStart(&appStart), GL_NOTIFICATION_CONTINUE);

  CHttpModule* httpMod = nullptr;
  ASSERT_EQ(mod.factory->GetHttpModule(&httpMod, nullptr), S_OK);

  // One byte past the 8 MiB the module is willing to buffer.
  std::string html = "<html><head><title>t</title></head><body>";
  html.resize(8 * 1024 * 1024 + 1, 'a');

  MockHttpContext httpCtx;
  MockHttpResponse httpResp;
  MockMetadataInfo metadata(L"MACHINE/WEBROOT/APPHOST/Default Web Site");
  PreparedChunkedResponse pr;
  setupChunkedHtmlResponse(httpCtx, httpResp, &app, &metadata, pr, {html});

  EXPECT_CALL(httpResp, SetHeader(Matcher<PCSTR>(StrEq(k_injection_header)), _, _, _)).Times(0);
  EXPECT_CALL(httpResp, Flush(_, _, _, _)).Times(0);

  EXPECT_EQ(httpMod->OnSendResponse(&httpCtx, nullptr), RQ_NOTIFICATION_CONTINUE);
  EXPECT_EQ(assembleResponseBody(pr), html);

  mod.factory->Terminate();
  mod.global->Terminate();
}

TEST(InjectionPipeline, DisabledConfig) {
  MockRumConfigElement rumConfig(false, {
                                            {L"applicationId", L"test-app-id"},
                                            {L"clientToken", L"test-client-token"},
                                        });
  MockAdminManager adminManager(&rumConfig);
  MockServer mockServer;
  EXPECT_CALL(mockServer, GetAdminManager()).WillRepeatedly(Return(&adminManager));

  auto mod = registerModule(mockServer);

  MockAppWithContext app;
  MockAppStartProvider appStart(app);
  mod.global->OnGlobalApplicationStart(&appStart);

  auto* ctx =
      static_cast<ModuleContext*>(app.GetModuleContextContainer()->GetModuleContext(nullptr));
  ASSERT_NE(ctx, nullptr);
  // When disabled, no snippet should be created.
  EXPECT_EQ(ctx->snippet, nullptr);

  CHttpModule* httpMod = nullptr;
  mod.factory->GetHttpModule(&httpMod, nullptr);

  MockHttpContext httpCtx;
  MockHttpResponse httpResp;
  MockMetadataInfo metadata(L"MACHINE/WEBROOT/APPHOST/Default Web Site");
  PreparedResponse pr;
  const char* html = "<html><head></head><body>Hello</body></html>";
  setupHtmlResponse(httpCtx, httpResp, &app, &metadata, pr, html);

  // OnSendResponse should skip injection (CONTINUE, not FINISH_REQUEST).
  EXPECT_EQ(httpMod->OnSendResponse(&httpCtx, nullptr), RQ_NOTIFICATION_CONTINUE);

  // Body unchanged.
  const std::string output(static_cast<const char*>(pr.chunk.FromMemory.pBuffer),
                           pr.chunk.FromMemory.BufferLength);
  EXPECT_EQ(output, html);

  mod.factory->Terminate();
  mod.global->Terminate();
}

// Per-location config: two different metadata paths within the same application
// get different config. The admin manager returns different config depending on
// the config path, and IMetadataInfo::GetMetaPath() provides the resolved path.
TEST(InjectionPipeline, PerLocationConfig) {
  // Path A: RUM enabled with service "service-a"
  MockRumConfigElement configA(true, {
                                         {L"applicationId", L"test-app-id"},
                                         {L"clientToken", L"test-client-token"},
                                         {L"service", L"service-a"},
                                         {L"sessionSampleRate", L"100"},
                                     });
  // Path B: RUM disabled
  MockRumConfigElement configB(false, {});

  std::map<std::wstring, MockRumConfigElement*> configs;
  configs[L"MACHINE/WEBROOT/APPHOST/Default Web Site"] = &configA;
  configs[L"MACHINE/WEBROOT/APPHOST/Default Web Site/api"] = &configB;
  MockAdminManager adminManager(configs);

  MockServer mockServer;
  EXPECT_CALL(mockServer, GetAdminManager()).WillRepeatedly(Return(&adminManager));

  auto mod = registerModule(mockServer);

  // Single application — both paths are within the same app.
  MockAppWithContext app(L"MACHINE/WEBROOT/APPHOST/Default Web Site");
  MockAppStartProvider appStart(app);
  mod.global->OnGlobalApplicationStart(&appStart);

  CHttpModule* httpMod = nullptr;
  mod.factory->GetHttpModule(&httpMod, nullptr);

  const char* html =
      "<html><head><title>Test</title></head>"
      "<body>Hello</body></html>";

  // Request 1: metadata path = site root → should inject with service-a
  {
    MockHttpContext httpCtx;
    MockHttpResponse httpResp;
    MockMetadataInfo metadata(L"MACHINE/WEBROOT/APPHOST/Default Web Site");
    PreparedResponse pr;
    setupHtmlResponse(httpCtx, httpResp, &app, &metadata, pr, html);
    EXPECT_CALL(httpResp, SetHeader(Matcher<PCSTR>(StrEq(k_injection_header)), _, _, _))
        .WillOnce(Return(S_OK));
    EXPECT_CALL(httpResp, Flush(_, _, _, _)).WillOnce(Return(S_OK));

    EXPECT_EQ(httpMod->OnSendResponse(&httpCtx, nullptr), RQ_NOTIFICATION_FINISH_REQUEST);

    const std::string output(static_cast<const char*>(pr.chunk.FromMemory.pBuffer),
                             pr.chunk.FromMemory.BufferLength);
    EXPECT_NE(output.find("service-a"), std::string::npos)
        << "Root path should inject with service-a, got:\n"
        << output;
  }

  // Request 2: metadata path = /api → should NOT inject (location override)
  {
    MockHttpContext httpCtx;
    MockHttpResponse httpResp;
    MockMetadataInfo metadata(L"MACHINE/WEBROOT/APPHOST/Default Web Site/api");
    PreparedResponse pr;
    setupHtmlResponse(httpCtx, httpResp, &app, &metadata, pr, html);

    EXPECT_EQ(httpMod->OnSendResponse(&httpCtx, nullptr), RQ_NOTIFICATION_CONTINUE)
        << "/api path should skip injection (location override disabled RUM)";

    const std::string output(static_cast<const char*>(pr.chunk.FromMemory.pBuffer),
                             pr.chunk.FromMemory.BufferLength);
    EXPECT_EQ(output, html) << "/api path response should be unchanged";
  }

  mod.factory->Terminate();
  mod.global->Terminate();
}

// ---------------------------------------------------------------------------
// Edge-case and error-path tests
// ---------------------------------------------------------------------------

// Cache hit: second request to the same metadata path uses cached context.
TEST(InjectionPipeline, MetadataCacheHit) {
  MockRumConfigElement rumConfig;
  MockAdminManager adminManager(&rumConfig);
  MockServer mockServer;
  EXPECT_CALL(mockServer, GetAdminManager()).WillRepeatedly(Return(&adminManager));

  auto mod = registerModule(mockServer);
  MockAppWithContext app;
  MockAppStartProvider appStart(app);
  mod.global->OnGlobalApplicationStart(&appStart);

  CHttpModule* httpMod = nullptr;
  mod.factory->GetHttpModule(&httpMod, nullptr);

  const char* html = "<html><head></head><body>Hello</body></html>";
  MockMetadataInfo metadata(L"MACHINE/WEBROOT/APPHOST/Default Web Site");

  // First request — cache miss, populates metadata container.
  {
    MockHttpContext httpCtx;
    MockHttpResponse httpResp;
    PreparedResponse pr;
    setupHtmlResponse(httpCtx, httpResp, &app, &metadata, pr, html);
    EXPECT_CALL(httpResp, SetHeader(Matcher<PCSTR>(StrEq(k_injection_header)), _, _, _))
        .WillOnce(Return(S_OK));
    EXPECT_CALL(httpResp, Flush(_, _, _, _)).WillOnce(Return(S_OK));
    EXPECT_EQ(httpMod->OnSendResponse(&httpCtx, nullptr), RQ_NOTIFICATION_FINISH_REQUEST);
  }

  // Second request — cache hit, same metadata object.
  {
    MockHttpContext httpCtx;
    MockHttpResponse httpResp;
    PreparedResponse pr;
    setupHtmlResponse(httpCtx, httpResp, &app, &metadata, pr, html);
    EXPECT_CALL(httpResp, SetHeader(Matcher<PCSTR>(StrEq(k_injection_header)), _, _, _))
        .WillOnce(Return(S_OK));
    EXPECT_CALL(httpResp, Flush(_, _, _, _)).WillOnce(Return(S_OK));
    EXPECT_EQ(httpMod->OnSendResponse(&httpCtx, nullptr), RQ_NOTIFICATION_FINISH_REQUEST);

    const std::string output(static_cast<const char*>(pr.chunk.FromMemory.pBuffer),
                             pr.chunk.FromMemory.BufferLength);
    EXPECT_NE(output.find("DD_RUM"), std::string::npos);
  }

  mod.factory->Terminate();
  mod.global->Terminate();
}

// SetModuleContext returns ERROR_ALREADY_ASSIGNED (race condition).
TEST(InjectionPipeline, SetModuleContextRace) {
  MockRumConfigElement rumConfig;
  MockAdminManager adminManager(&rumConfig);
  MockServer mockServer;
  EXPECT_CALL(mockServer, GetAdminManager()).WillRepeatedly(Return(&adminManager));

  auto mod = registerModule(mockServer);
  MockAppWithContext app;
  MockAppStartProvider appStart(app);
  mod.global->OnGlobalApplicationStart(&appStart);

  CHttpModule* httpMod = nullptr;
  mod.factory->GetHttpModule(&httpMod, nullptr);

  // Simulate the race the production code is trying to handle: this thread's
  // GetModuleContext returns null (cache miss), so it builds new_ctx and
  // calls SetModuleContext, which is rejected with ERROR_ALREADY_ASSIGNED
  // because another thread inserted winner_ctx first. The second
  // GetModuleContext then returns winner_ctx.
  MockMetadataInfo metadata(L"MACHINE/WEBROOT/APPHOST/Default Web Site");
  auto* winner_ctx = new ModuleContext;
  auto* app_ctx =
      static_cast<ModuleContext*>(app.GetModuleContextContainer()->GetModuleContext(nullptr));
  winner_ctx->logger = app_ctx->logger;
  winner_ctx->snippet = snippet_create_from_json(
      R"({"majorVersion":5,"rum":{"applicationId":"test-app-id","clientToken":"test-client-token","service":"test-service","sessionSampleRate":100}})");
  ASSERT_NE(winner_ctx->snippet, nullptr);
  ASSERT_EQ(winner_ctx->snippet->error_code, 0u);
  metadata.Container().SimulateRaceWith(winner_ctx);

  const char* html = "<html><head></head><body>Hello</body></html>";
  MockHttpContext httpCtx;
  MockHttpResponse httpResp;
  PreparedResponse pr;
  setupHtmlResponse(httpCtx, httpResp, &app, &metadata, pr, html);
  EXPECT_CALL(httpResp, SetHeader(Matcher<PCSTR>(StrEq(k_injection_header)), _, _, _))
      .WillOnce(Return(S_OK));
  EXPECT_CALL(httpResp, Flush(_, _, _, _)).WillOnce(Return(S_OK));

  EXPECT_EQ(httpMod->OnSendResponse(&httpCtx, nullptr), RQ_NOTIFICATION_FINISH_REQUEST);

  // winner_ctx is owned by metadata.Container() and freed by its destructor.

  mod.factory->Terminate();
  mod.global->Terminate();
}

// SetModuleContext returns a generic failure — no injection, no crash.
TEST(InjectionPipeline, SetModuleContextFailure) {
  MockRumConfigElement rumConfig;
  MockAdminManager adminManager(&rumConfig);
  MockServer mockServer;
  EXPECT_CALL(mockServer, GetAdminManager()).WillRepeatedly(Return(&adminManager));

  auto mod = registerModule(mockServer);
  MockAppWithContext app;
  MockAppStartProvider appStart(app);
  mod.global->OnGlobalApplicationStart(&appStart);

  CHttpModule* httpMod = nullptr;
  mod.factory->GetHttpModule(&httpMod, nullptr);

  MockMetadataInfo metadata(L"MACHINE/WEBROOT/APPHOST/Default Web Site");
  metadata.Container().SetModuleContextResult(E_FAIL);

  const char* html = "<html><head></head><body>Hello</body></html>";
  MockHttpContext httpCtx;
  MockHttpResponse httpResp;
  PreparedResponse pr;
  setupHtmlResponse(httpCtx, httpResp, &app, &metadata, pr, html);

  EXPECT_EQ(httpMod->OnSendResponse(&httpCtx, nullptr), RQ_NOTIFICATION_CONTINUE);

  const std::string output(static_cast<const char*>(pr.chunk.FromMemory.pBuffer),
                           pr.chunk.FromMemory.BufferLength);
  EXPECT_EQ(output, html);

  mod.factory->Terminate();
  mod.global->Terminate();
}

// Null metadata — falls back to app-level context and injects.
TEST(InjectionPipeline, NullMetadataFallback) {
  MockRumConfigElement rumConfig;
  MockAdminManager adminManager(&rumConfig);
  MockServer mockServer;
  EXPECT_CALL(mockServer, GetAdminManager()).WillRepeatedly(Return(&adminManager));

  auto mod = registerModule(mockServer);
  MockAppWithContext app;
  MockAppStartProvider appStart(app);
  mod.global->OnGlobalApplicationStart(&appStart);

  CHttpModule* httpMod = nullptr;
  mod.factory->GetHttpModule(&httpMod, nullptr);

  const char* html = "<html><head></head><body>Hello</body></html>";
  MockHttpContext httpCtx;
  MockHttpResponse httpResp;
  PreparedResponse pr;
  setupHtmlResponse(httpCtx, httpResp, &app, nullptr, pr, html);
  EXPECT_CALL(httpResp, SetHeader(Matcher<PCSTR>(StrEq(k_injection_header)), _, _, _))
      .WillOnce(Return(S_OK));
  EXPECT_CALL(httpResp, Flush(_, _, _, _)).WillOnce(Return(S_OK));

  EXPECT_EQ(httpMod->OnSendResponse(&httpCtx, nullptr), RQ_NOTIFICATION_FINISH_REQUEST);

  mod.factory->Terminate();
  mod.global->Terminate();
}

// Null metadata context container — falls back to app-level context and
// injects.
TEST(InjectionPipeline, NullMetadataContainerFallback) {
  MockRumConfigElement rumConfig;
  MockAdminManager adminManager(&rumConfig);
  MockServer mockServer;
  EXPECT_CALL(mockServer, GetAdminManager()).WillRepeatedly(Return(&adminManager));

  auto mod = registerModule(mockServer);
  MockAppWithContext app;
  MockAppStartProvider appStart(app);
  mod.global->OnGlobalApplicationStart(&appStart);

  CHttpModule* httpMod = nullptr;
  mod.factory->GetHttpModule(&httpMod, nullptr);

  MockMetadataInfo metadata(L"MACHINE/WEBROOT/APPHOST/Default Web Site");
  metadata.SetReturnNullContainer(true);

  const char* html = "<html><head></head><body>Hello</body></html>";
  MockHttpContext httpCtx;
  MockHttpResponse httpResp;
  PreparedResponse pr;
  setupHtmlResponse(httpCtx, httpResp, &app, &metadata, pr, html);
  EXPECT_CALL(httpResp, SetHeader(Matcher<PCSTR>(StrEq(k_injection_header)), _, _, _))
      .WillOnce(Return(S_OK));
  EXPECT_CALL(httpResp, Flush(_, _, _, _)).WillOnce(Return(S_OK));

  // Falls back to app-level context and injects.
  EXPECT_EQ(httpMod->OnSendResponse(&httpCtx, nullptr), RQ_NOTIFICATION_FINISH_REQUEST);

  mod.factory->Terminate();
  mod.global->Terminate();
}

// CleanupStoredContext properly frees snippet.
TEST(InjectionPipeline, CleanupStoredContextFreesSnippet) {
  auto* ctx = new ModuleContext;
  ctx->snippet = snippet_create_from_json(
      R"({"majorVersion":5,"rum":{"applicationId":"test","clientToken":"tok"}})");
  ASSERT_NE(ctx->snippet, nullptr);

  // Under ASan this catches use-after-free or double-free.
  ctx->CleanupStoredContext();
}

// Two enabled locations with different services — verify per-path isolation.
TEST(InjectionPipeline, TwoEnabledLocationsDistinctConfig) {
  MockRumConfigElement configA(true, {
                                         {L"applicationId", L"app-a"},
                                         {L"clientToken", L"tok-a"},
                                         {L"service", L"svc-alpha"},
                                         {L"sessionSampleRate", L"100"},
                                     });
  MockRumConfigElement configB(true, {
                                         {L"applicationId", L"app-b"},
                                         {L"clientToken", L"tok-b"},
                                         {L"service", L"svc-beta"},
                                         {L"sessionSampleRate", L"100"},
                                     });

  std::map<std::wstring, MockRumConfigElement*> configs;
  configs[L"MACHINE/WEBROOT/APPHOST/Default Web Site"] = &configA;
  configs[L"MACHINE/WEBROOT/APPHOST/Default Web Site/blog"] = &configB;
  MockAdminManager adminManager(configs);

  MockServer mockServer;
  EXPECT_CALL(mockServer, GetAdminManager()).WillRepeatedly(Return(&adminManager));

  auto mod = registerModule(mockServer);
  MockAppWithContext app(L"MACHINE/WEBROOT/APPHOST/Default Web Site");
  MockAppStartProvider appStart(app);
  mod.global->OnGlobalApplicationStart(&appStart);

  CHttpModule* httpMod = nullptr;
  mod.factory->GetHttpModule(&httpMod, nullptr);

  const char* html = "<html><head></head><body>Hello</body></html>";

  // Root — svc-alpha
  {
    MockHttpContext httpCtx;
    MockHttpResponse httpResp;
    MockMetadataInfo metadata(L"MACHINE/WEBROOT/APPHOST/Default Web Site");
    PreparedResponse pr;
    setupHtmlResponse(httpCtx, httpResp, &app, &metadata, pr, html);
    EXPECT_CALL(httpResp, SetHeader(Matcher<PCSTR>(StrEq(k_injection_header)), _, _, _))
        .WillOnce(Return(S_OK));
    EXPECT_CALL(httpResp, Flush(_, _, _, _)).WillOnce(Return(S_OK));

    EXPECT_EQ(httpMod->OnSendResponse(&httpCtx, nullptr), RQ_NOTIFICATION_FINISH_REQUEST);
    const std::string output(static_cast<const char*>(pr.chunk.FromMemory.pBuffer),
                             pr.chunk.FromMemory.BufferLength);
    EXPECT_NE(output.find("svc-alpha"), std::string::npos);
    EXPECT_EQ(output.find("svc-beta"), std::string::npos);
  }

  // /blog — svc-beta
  {
    MockHttpContext httpCtx;
    MockHttpResponse httpResp;
    MockMetadataInfo metadata(L"MACHINE/WEBROOT/APPHOST/Default Web Site/blog");
    PreparedResponse pr;
    setupHtmlResponse(httpCtx, httpResp, &app, &metadata, pr, html);
    EXPECT_CALL(httpResp, SetHeader(Matcher<PCSTR>(StrEq(k_injection_header)), _, _, _))
        .WillOnce(Return(S_OK));
    EXPECT_CALL(httpResp, Flush(_, _, _, _)).WillOnce(Return(S_OK));

    EXPECT_EQ(httpMod->OnSendResponse(&httpCtx, nullptr), RQ_NOTIFICATION_FINISH_REQUEST);
    const std::string output(static_cast<const char*>(pr.chunk.FromMemory.pBuffer),
                             pr.chunk.FromMemory.BufferLength);
    EXPECT_NE(output.find("svc-beta"), std::string::npos);
    EXPECT_EQ(output.find("svc-alpha"), std::string::npos);
  }

  mod.factory->Terminate();
  mod.global->Terminate();
}

// OnBeginRequest uses per-location cache: pending header should NOT be set
// for a path where RUM is disabled via <location> override (after cache is
// populated by a prior OnSendResponse).
TEST(InjectionPipeline, OnBeginRequestRespectsLocationOverride) {
  MockRumConfigElement configA(true, {
                                         {L"applicationId", L"test-app-id"},
                                         {L"clientToken", L"test-client-token"},
                                         {L"service", L"service-a"},
                                         {L"sessionSampleRate", L"100"},
                                     });
  MockRumConfigElement configB(false, {});

  std::map<std::wstring, MockRumConfigElement*> configs;
  configs[L"MACHINE/WEBROOT/APPHOST/Default Web Site"] = &configA;
  configs[L"MACHINE/WEBROOT/APPHOST/Default Web Site/api"] = &configB;
  MockAdminManager adminManager(configs);

  MockServer mockServer;
  EXPECT_CALL(mockServer, GetAdminManager()).WillRepeatedly(Return(&adminManager));

  auto mod = registerModule(mockServer);
  MockAppWithContext app(L"MACHINE/WEBROOT/APPHOST/Default Web Site");
  MockAppStartProvider appStart(app);
  mod.global->OnGlobalApplicationStart(&appStart);

  CHttpModule* httpMod = nullptr;
  mod.factory->GetHttpModule(&httpMod, nullptr);

  const char* html = "<html><head></head><body>Hello</body></html>";

  // First: OnSendResponse to /api to populate the metadata cache (disabled).
  MockMetadataInfo apiMetadata(L"MACHINE/WEBROOT/APPHOST/Default Web Site/api");
  {
    MockHttpContext httpCtx;
    MockHttpResponse httpResp;
    PreparedResponse pr;
    setupHtmlResponse(httpCtx, httpResp, &app, &apiMetadata, pr, html);
    EXPECT_EQ(httpMod->OnSendResponse(&httpCtx, nullptr), RQ_NOTIFICATION_CONTINUE);
  }

  // OnBeginRequest to /api — pending header must NOT be set (location is disabled).
  {
    MockHttpContext httpCtx;
    MockHttpRequest httpReq;
    MockEventProvider eventProvider;
    EXPECT_CALL(httpCtx, GetRequest()).WillRepeatedly(Return(&httpReq));
    EXPECT_CALL(httpCtx, GetApplication()).WillRepeatedly(Return(&app));
    EXPECT_CALL(httpCtx, GetMetadata()).WillRepeatedly(Return(&apiMetadata));

    // The pending header should NOT be set.
    EXPECT_CALL(httpReq, SetHeader(Matcher<PCSTR>(StrEq(k_injection_pending_header)), _, _, _))
        .Times(0);

    EXPECT_EQ(httpMod->OnBeginRequest(&httpCtx, &eventProvider), RQ_NOTIFICATION_CONTINUE);
  }

  // Contrast: OnBeginRequest to root — SHOULD set the pending header.
  MockMetadataInfo rootMetadata(L"MACHINE/WEBROOT/APPHOST/Default Web Site");
  {
    // First populate root cache via OnSendResponse.
    MockHttpContext httpCtx;
    MockHttpResponse httpResp;
    PreparedResponse pr;
    setupHtmlResponse(httpCtx, httpResp, &app, &rootMetadata, pr, html);
    EXPECT_CALL(httpResp, SetHeader(Matcher<PCSTR>(StrEq(k_injection_header)), _, _, _))
        .WillOnce(Return(S_OK));
    EXPECT_CALL(httpResp, Flush(_, _, _, _)).WillOnce(Return(S_OK));
    EXPECT_EQ(httpMod->OnSendResponse(&httpCtx, nullptr), RQ_NOTIFICATION_FINISH_REQUEST);
  }
  {
    MockHttpContext httpCtx;
    MockHttpRequest httpReq;
    MockEventProvider eventProvider;
    EXPECT_CALL(httpCtx, GetRequest()).WillRepeatedly(Return(&httpReq));
    EXPECT_CALL(httpCtx, GetApplication()).WillRepeatedly(Return(&app));
    EXPECT_CALL(httpCtx, GetMetadata()).WillRepeatedly(Return(&rootMetadata));

    EXPECT_CALL(httpReq, SetHeader(Matcher<PCSTR>(StrEq(k_injection_pending_header)), _, _, _))
        .WillOnce(Return(S_OK));

    EXPECT_EQ(httpMod->OnBeginRequest(&httpCtx, &eventProvider), RQ_NOTIFICATION_CONTINUE);
  }

  mod.factory->Terminate();
  mod.global->Terminate();
}

// MissingConfig: no datadogRum section at all — should not crash or inject.
TEST(InjectionPipeline, MissingConfig) {
  // Admin manager that returns E_FAIL for all paths — no datadogRum config.
  const std::map<std::wstring, MockRumConfigElement*> empty;
  MockAdminManager adminManager(empty);
  MockServer mockServer;
  EXPECT_CALL(mockServer, GetAdminManager()).WillRepeatedly(Return(&adminManager));

  auto mod = registerModule(mockServer);

  MockAppWithContext app;
  MockAppStartProvider appStart(app);
  mod.global->OnGlobalApplicationStart(&appStart);

  auto* ctx =
      static_cast<ModuleContext*>(app.GetModuleContextContainer()->GetModuleContext(nullptr));
  ASSERT_NE(ctx, nullptr);
  EXPECT_EQ(ctx->snippet, nullptr);

  CHttpModule* httpMod = nullptr;
  mod.factory->GetHttpModule(&httpMod, nullptr);

  MockHttpContext httpCtx;
  MockHttpResponse httpResp;
  MockMetadataInfo metadata(L"MACHINE/WEBROOT/APPHOST/Default Web Site");
  PreparedResponse pr;
  const char* html = "<html><head></head><body>Hello</body></html>";
  setupHtmlResponse(httpCtx, httpResp, &app, &metadata, pr, html);

  EXPECT_EQ(httpMod->OnSendResponse(&httpCtx, nullptr), RQ_NOTIFICATION_CONTINUE);

  const std::string output(static_cast<const char*>(pr.chunk.FromMemory.pBuffer),
                           pr.chunk.FromMemory.BufferLength);
  EXPECT_EQ(output, html);

  mod.factory->Terminate();
  mod.global->Terminate();
}

// get_Collection fails on initial load — no prior snippet to keep, so snippet
// stays nullptr (no injection).
TEST(InjectionPipeline, GetCollectionFailureOnFirstLoad) {
  MockRumConfigElement config(true, {{L"applicationId", L"test-app-id"},
                                     {L"clientToken", L"test-client-token"},
                                     {L"service", L"test-service"},
                                     {L"sessionSampleRate", L"100"}});
  config.SetCollectionResult(E_OUTOFMEMORY);

  MockAdminManager adminManager(&config);
  MockServer mockServer;
  EXPECT_CALL(mockServer, GetAdminManager()).WillRepeatedly(Return(&adminManager));

  auto mod = registerModule(mockServer);

  MockAppWithContext app;
  MockAppStartProvider appStart(app);
  mod.global->OnGlobalApplicationStart(&appStart);

  auto* ctx =
      static_cast<ModuleContext*>(app.GetModuleContextContainer()->GetModuleContext(nullptr));
  ASSERT_NE(ctx, nullptr);
  // First load has no prior snippet — transient error keeps nullptr.
  EXPECT_EQ(ctx->snippet, nullptr);

  mod.factory->Terminate();
  mod.global->Terminate();
}

// get_Collection fails during config reload — existing snippet is preserved
// so injection self-heals on the next successful reload.
TEST(InjectionPipeline, GetCollectionFailurePreservesExistingConfig) {
  // First: load config successfully.
  MockRumConfigElement config(true, {{L"applicationId", L"test-app-id"},
                                     {L"clientToken", L"test-client-token"},
                                     {L"service", L"test-service"},
                                     {L"sessionSampleRate", L"100"}});

  MockAdminManager adminManager(&config);
  MockServer mockServer;
  EXPECT_CALL(mockServer, GetAdminManager()).WillRepeatedly(Return(&adminManager));

  auto mod = registerModule(mockServer);

  MockAppWithContext app(L"MACHINE/WEBROOT/APPHOST/Default Web Site");
  MockAppStartProvider appStart(app);
  mod.global->OnGlobalApplicationStart(&appStart);

  auto* ctx =
      static_cast<ModuleContext*>(app.GetModuleContextContainer()->GetModuleContext(nullptr));
  ASSERT_NE(ctx, nullptr);
  ASSERT_NE(ctx->snippet, nullptr);
  auto* original_snippet = ctx->snippet;

  config.SetCollectionResult(E_OUTOFMEMORY);

  MockConfigChangeProvider configChange(L"MACHINE/WEBROOT/APPHOST/Default Web Site");
  mod.global->OnGlobalConfigurationChange(&configChange);

  // Existing snippet should be preserved — not cleared by the transient error.
  EXPECT_EQ(ctx->snippet, original_snippet);

  mod.factory->Terminate();
  mod.global->Terminate();
}

// 'enabled' attribute lookup fails on initial load — get_property cannot
// distinguish a missing attribute from a COM failure, so the read is treated
// as Error. With no prior snippet, snippet stays nullptr.
TEST(InjectionPipeline, MissingEnabledAttributeOnFirstLoad) {
  MockRumConfigElement config;
  config.SetEnabledPropertyMissing(true);

  MockAdminManager adminManager(&config);
  MockServer mockServer;
  EXPECT_CALL(mockServer, GetAdminManager()).WillRepeatedly(Return(&adminManager));

  auto mod = registerModule(mockServer);

  MockAppWithContext app;
  MockAppStartProvider appStart(app);
  mod.global->OnGlobalApplicationStart(&appStart);

  auto* ctx =
      static_cast<ModuleContext*>(app.GetModuleContextContainer()->GetModuleContext(nullptr));
  ASSERT_NE(ctx, nullptr);
  EXPECT_EQ(ctx->snippet, nullptr);

  mod.factory->Terminate();
  mod.global->Terminate();
}

// A reload whose config fails make_snippet validation must preserve the
// previous valid snippet — operators rely on last-known-good injection
// continuing while a broken deploy is investigated.
TEST(InjectionPipeline, InvalidConfigOnReloadPreservesExistingSnippet) {
  MockRumConfigElement valid;

  MockAdminManager adminManager(&valid);
  MockServer mockServer;
  EXPECT_CALL(mockServer, GetAdminManager()).WillRepeatedly(Return(&adminManager));

  auto mod = registerModule(mockServer);

  MockAppWithContext app(L"MACHINE/WEBROOT/APPHOST/Default Web Site");
  MockAppStartProvider appStart(app);
  mod.global->OnGlobalApplicationStart(&appStart);

  auto* ctx =
      static_cast<ModuleContext*>(app.GetModuleContextContainer()->GetModuleContext(nullptr));
  ASSERT_NE(ctx, nullptr);
  ASSERT_NE(ctx->snippet, nullptr);
  auto* original_snippet = ctx->snippet;

  // sessionSampleRate=200 is out of the 0..100 range → error_code 4.
  MockRumConfigElement invalid(true, {{L"applicationId", L"test-app-id"},
                                      {L"clientToken", L"test-client-token"},
                                      {L"site", L"datadoghq.com"},
                                      {L"service", L"test-service"},
                                      {L"sessionSampleRate", L"200"}});
  adminManager.SetConfig(&invalid);

  MockConfigChangeProvider configChange(L"MACHINE/WEBROOT/APPHOST/Default Web Site");
  mod.global->OnGlobalConfigurationChange(&configChange);

  EXPECT_EQ(ctx->snippet, original_snippet);

  mod.factory->Terminate();
  mod.global->Terminate();
}

// Error semantics on the 'enabled' read must not wipe an active snippet.
TEST(InjectionPipeline, MissingEnabledAttributePreservesExistingConfig) {
  MockRumConfigElement config;

  MockAdminManager adminManager(&config);
  MockServer mockServer;
  EXPECT_CALL(mockServer, GetAdminManager()).WillRepeatedly(Return(&adminManager));

  auto mod = registerModule(mockServer);

  MockAppWithContext app(L"MACHINE/WEBROOT/APPHOST/Default Web Site");
  MockAppStartProvider appStart(app);
  mod.global->OnGlobalApplicationStart(&appStart);

  auto* ctx =
      static_cast<ModuleContext*>(app.GetModuleContextContainer()->GetModuleContext(nullptr));
  ASSERT_NE(ctx, nullptr);
  ASSERT_NE(ctx->snippet, nullptr);
  auto* original_snippet = ctx->snippet;

  config.SetEnabledPropertyMissing(true);

  MockConfigChangeProvider configChange(L"MACHINE/WEBROOT/APPHOST/Default Web Site");
  mod.global->OnGlobalConfigurationChange(&configChange);

  EXPECT_EQ(ctx->snippet, original_snippet);

  mod.factory->Terminate();
  mod.global->Terminate();
}
