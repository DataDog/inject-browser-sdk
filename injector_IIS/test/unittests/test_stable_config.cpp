// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2024-Present Datadog, Inc.

// Covers the stable-configuration path: RUM settings sourced from
// application_monitoring.yaml rather than from a per-site IIS section.
//
// Two tiers:
//   * make_config_json  — pure, no COM, no I/O. Pins the per-key typing and the
//                         overlay shape the Rust side accepts.
//   * update_module_context — driven with a real StableConfig handle read from a
//                         temp-directory YAML file via stable_config_read_from_paths,
//                         so the whole IIS-overlay-on-stable-config merge is
//                         exercised end to end.

#include "config_reader.h"
#include "pipeline_mocks.h"
#include "telemetry.h"

#include <rapidjson/document.h>

#include <cstdlib>
#include <filesystem>
#include <fstream>
#include <memory>
#include <optional>
#include <set>
#include <string>
#include <unordered_map>

using namespace datadog::rum;

namespace {

// A logger that swallows output; the tests assert on behaviour, not on logs.
class NullLogger : public Logger {
 public:
  NullLogger() : Logger(LogLevel::Debug) {}
};

using Opts = std::unordered_map<std::string, std::string>;

rapidjson::Document parse(const std::string& json) {
  rapidjson::Document doc;
  doc.Parse(json.c_str());
  EXPECT_FALSE(doc.HasParseError()) << "make_config_json emitted invalid JSON: " << json;
  return doc;
}

// ---------------------------------------------------------------------------
// Temp-directory YAML fixture
// ---------------------------------------------------------------------------

// Writes application_monitoring.yaml files into a unique temp directory and
// hands out a StableConfig handle read from them. Uses
// stable_config_read_from_paths so nothing touches C:\ProgramData\Datadog.
class StableConfigFixture {
  std::filesystem::path dir_;

 public:
  explicit StableConfigFixture(const std::string& name) {
    dir_ = std::filesystem::temp_directory_path() /
           ("iis-stable-config-" + name + "-" + std::to_string(::GetCurrentProcessId()));
    std::filesystem::remove_all(dir_);
    std::filesystem::create_directories(dir_);
  }

  ~StableConfigFixture() {
    std::error_code ec;
    std::filesystem::remove_all(dir_, ec);
  }

  StableConfigFixture(const StableConfigFixture&) = delete;
  StableConfigFixture& operator=(const StableConfigFixture&) = delete;

  // NOLINTNEXTLINE(bugprone-easily-swappable-parameters)
  std::string write(const std::string& name, const std::string& contents) {
    const auto path = dir_ / name;
    std::ofstream out(path, std::ios::binary);
    out << contents;
    out.close();
    return path.string();
  }

  std::string absent(const std::string& name) const { return (dir_ / name).string(); }

  // Reads a handle from `local` (and optionally `fleet`). Both may be paths to
  // files that don't exist, which models a host with no stable configuration.
  std::unique_ptr<::StableConfig, StableConfigDeleter> read(const std::string& local,
                                                            const std::string& fleet) {
    return std::unique_ptr<::StableConfig, StableConfigDeleter>(stable_config_read_from_paths(
        k_stable_config_language, /* debug_logs */ false, local.c_str(), fleet.c_str()));
  }
};

// A minimal rule matching language=browser-sdk with the given DD_RUM_* keys.
std::string browser_sdk_rule(const std::string& keys) {
  return "rules:\n"
         "- selectors:\n"
         "  - origin: language\n"
         "    matches: [\"browser-sdk\"]\n"
         "    operator: equals\n"
         "  configuration:\n" +
         keys;
}

constexpr PCWSTR k_site_path = L"MACHINE/WEBROOT/APPHOST/Default Web Site";

// The snippet has no content accessor, so run it through the injector to read
// what would actually reach the browser.
std::string inject(const Snippet* snippet) {
  static constexpr std::string_view k_html = "<html><head></head><body>Hello</body></html>";

  Injector* injector = injector_create(snippet);
  if (injector == nullptr) return {};

  std::string out;
  auto append = [&out](const InjectorResult& result) {
    for (uintptr_t i = 0; i < result.slices_length; ++i) {
      const auto& slice = result.slices[i];
      out.append(reinterpret_cast<const char*>(slice.start), slice.length);
    }
  };
  append(injector_write(injector, reinterpret_cast<const uint8_t*>(k_html.data()),
                        static_cast<uint32_t>(k_html.size())));
  append(injector_end(injector));
  injector_cleanup(injector);
  return out;
}

// Drives update_module_context against a mock admin manager and keeps everything
// the resulting context borrows alive for as long as the caller holds it.
class Resolved {
  MockServer server_;
  ModuleContext ctx_;

 public:
  Resolved(MockAdminManager& admin, const ::StableConfig* stable_config, Logger& logger,
           PCWSTR path) {
    EXPECT_CALL(server_, GetAdminManager()).WillRepeatedly(Return(&admin));
    ctx_.logger = &logger;
    ctx_.server = &server_;
    ctx_.stable_config = stable_config;
    update_module_context(ctx_, server_, path, logger, stable_config);
  }

  // Immovable: ctx_ borrows server_.
  Resolved(const Resolved&) = delete;
  Resolved& operator=(const Resolved&) = delete;

  const ModuleContext& ctx() const { return ctx_; }
  bool injects() const { return ctx_.snippet != nullptr; }
  std::string injected() const { return inject(ctx_.snippet); }
};

Resolved resolve(MockAdminManager& admin, const ::StableConfig* stable_config, Logger& logger,
                 PCWSTR path = k_site_path) {
  return {admin, stable_config, logger, path};
}

}  // namespace

// ---------------------------------------------------------------------------
// Telemetry emission guard
// ---------------------------------------------------------------------------

#if GTEST_HAS_DEATH_TEST
TEST(TelemetryGuardDeathTest, OversizedResponseBeforeInitIsSafe) {
  // Run in a fresh process: earlier integration tests initialize telemetry and
  // would otherwise conceal an unguarded emission on this request path.
  ASSERT_EXIT(
      {
        const int status = [] {
          NullLogger logger;
          ModuleContext ctx;
          ctx.logger = &logger;
          ctx.injection_strategy = InjectionStrategy::HeadOpenTagWithCloseTagFallback;
          ctx.snippet = snippet_create_from_json(
              R"({"majorVersion":6,"rum":{"applicationId":"app","clientToken":"token"}})");
          if (ctx.snippet == nullptr || ctx.snippet->error_code != 0) return 1;

          std::string html = "<html><head></head><body>";
          html.resize(8 * 1024 * 1024 + 1, 'a');
          MockHttpContext http_ctx;
          MockHttpResponse http_resp;
          MockAppWithContext app;
          MockMetadataInfo metadata(k_site_path);
          PreparedResponse response;
          setupHtmlResponse(http_ctx, http_resp, &app, &metadata, response, html.c_str());
          HttpModule module;
          const auto result = module.PerformInjection(http_ctx, http_resp, ctx);
          return result == RQ_NOTIFICATION_CONTINUE && response.raw.EntityChunkCount == 1 &&
                         response.chunk.FromMemory.pBuffer == html.data() &&
                         response.chunk.FromMemory.BufferLength == html.size()
                     ? 0
                     : 1;
        }();
        // Destroy the mocks and verify their expectations before exiting the
        // child; std::exit itself does not unwind local objects.
        std::exit(status);
      },
      ::testing::ExitedWithCode(0), "");
}
#endif

TEST(TelemetryGuard, EmittingBeforeInitIsSafe) {
  // Regression guard. dd-trace-cpp's telemetry instance is a function-local
  // static constructed from an Optional it dereferences unconditionally:
  //
  //     static TelemetryProxy telemetry(make_telemetry(*init));
  //
  // so the first-ever call to counter::increment, if it lands before init(),
  // dereferences an empty Optional. On a real IIS host that took down the w3wp
  // worker process with an access violation every time a malformed
  // application_monitoring.yaml made load_stable_config report config_invalid.
  // Everything must emit through telemetry::increment, which drops the metric
  // until mark_ready() has run.
  telemetry::increment(telemetry::config_invalid,
                       telemetry::build_tags("error_code:6", std::string{}, std::string{}));
}

// ---------------------------------------------------------------------------
// Tier A: make_config_json
// ---------------------------------------------------------------------------

TEST(MakeConfigJson, OmitsMajorVersionAndEnabledWhenDisengaged) {
  NullLogger logger;
  const auto doc = parse(make_config_json(std::nullopt, std::nullopt, {}, logger));

  EXPECT_FALSE(doc.HasMember("majorVersion"))
      << "an omitted key is what lets stable config supply the value";
  EXPECT_FALSE(doc.HasMember("enabled"));
  ASSERT_TRUE(doc.HasMember("rum")) << "rum must always be present";
  EXPECT_TRUE(doc["rum"].IsObject());
}

TEST(MakeConfigJson, EmitsMajorVersionAndEnabledWhenEngaged) {
  NullLogger logger;
  const auto doc = parse(make_config_json(5, false, {}, logger));

  ASSERT_TRUE(doc.HasMember("majorVersion"));
  EXPECT_EQ(doc["majorVersion"].GetInt(), 5);
  ASSERT_TRUE(doc.HasMember("enabled"));
  EXPECT_FALSE(doc["enabled"].GetBool());
}

TEST(MakeConfigJson, TopLevelKeysStayWithinTheOverlaySchema) {
  // ConfigurationOverlay is deny_unknown_fields: one stray top-level key would
  // reject the whole overlay, so guard the emitted shape.
  NullLogger logger;
  const Opts opts{{"applicationId", "app"}, {"clientToken", "tok"}, {"majorVersion", "9"}};
  const auto doc = parse(make_config_json(6, true, opts, logger));

  const std::set<std::string> allowed{"majorVersion", "enabled", "rum"};
  for (auto it = doc.MemberBegin(); it != doc.MemberEnd(); ++it) {
    EXPECT_TRUE(allowed.count(it->name.GetString()) > 0)
        << "unexpected top-level key: " << it->name.GetString();
  }
  EXPECT_EQ(doc["majorVersion"].GetInt(), 6)
      << "a majorVersion SDK option must not shadow the section's version";
}

TEST(MakeConfigJson, SampleRatesBecomeNumbers) {
  NullLogger logger;
  const Opts opts{{"sessionSampleRate", "12.5"}, {"sessionReplaySampleRate", "0"}};
  const auto doc = parse(make_config_json(std::nullopt, std::nullopt, opts, logger));

  ASSERT_TRUE(doc["rum"]["sessionSampleRate"].IsNumber());
  EXPECT_DOUBLE_EQ(doc["rum"]["sessionSampleRate"].GetDouble(), 12.5);
  EXPECT_DOUBLE_EQ(doc["rum"]["sessionReplaySampleRate"].GetDouble(), 0.0);
}

TEST(MakeConfigJson, UnparseableSampleRateIsDropped) {
  NullLogger logger;
  const Opts opts{{"sessionSampleRate", "not-a-number"}, {"applicationId", "app"}};
  const auto doc = parse(make_config_json(std::nullopt, std::nullopt, opts, logger));

  EXPECT_FALSE(doc["rum"].HasMember("sessionSampleRate"))
      << "a garbage rate is dropped and logged, not passed through as a string";
  EXPECT_STREQ(doc["rum"]["applicationId"].GetString(), "app")
      << "one bad key must not discard the rest";
}

TEST(MakeConfigJson, TrackFlagsAreBooleansComparedAgainstTheLiteralTrue) {
  // Documents the existing `value == "true"` rule: anything else is false.
  NullLogger logger;
  const Opts opts{
      {"trackResources", "1"}, {"trackLongTasks", "true"}, {"trackUserInteractions", "True"}};
  const auto doc = parse(make_config_json(std::nullopt, std::nullopt, opts, logger));

  ASSERT_TRUE(doc["rum"]["trackResources"].IsBool());
  EXPECT_FALSE(doc["rum"]["trackResources"].GetBool());
  EXPECT_TRUE(doc["rum"]["trackLongTasks"].GetBool());
  EXPECT_FALSE(doc["rum"]["trackUserInteractions"].GetBool()) << "the comparison is case-sensitive";
}

TEST(MakeConfigJson, AllowedTracingUrlsSplitsAndTrims) {
  NullLogger logger;
  const Opts opts{{"allowedTracingUrls", "a, b ,c"}};
  const auto doc = parse(make_config_json(std::nullopt, std::nullopt, opts, logger));

  ASSERT_TRUE(doc["rum"]["allowedTracingUrls"].IsArray());
  const auto& urls = doc["rum"]["allowedTracingUrls"];
  ASSERT_EQ(urls.Size(), 3u);
  EXPECT_STREQ(urls[0].GetString(), "a");
  EXPECT_STREQ(urls[1].GetString(), "b");
  EXPECT_STREQ(urls[2].GetString(), "c");
}

TEST(MakeConfigJson, UnknownKeysPassThroughAsStrings) {
  // Forward-compat with new Browser SDK init options.
  NullLogger logger;
  const Opts opts{{"someFutureOption", "value"}};
  const auto doc = parse(make_config_json(std::nullopt, std::nullopt, opts, logger));

  EXPECT_STREQ(doc["rum"]["someFutureOption"].GetString(), "value");
}

// ---------------------------------------------------------------------------
// Tier B: update_module_context against real YAML
// ---------------------------------------------------------------------------

TEST(StableConfig, InjectsWithNoIisSectionAtAll) {
  StableConfigFixture fixture("only-stable");
  const auto local =
      fixture.write("local.yaml", browser_sdk_rule("    DD_RUM_APPLICATION_ID: app-stable\n"
                                                   "    DD_RUM_CLIENT_TOKEN: tok-stable\n"
                                                   "    DD_RUM_SITE: datadoghq.com\n"));
  auto config = fixture.read(local, fixture.absent("fleet.yaml"));
  ASSERT_NE(config, nullptr);
  ASSERT_EQ(config->error_code, 0) << config->error_message;

  // No datadogRum section anywhere: GetAdminSection fails for every path.
  const std::map<std::wstring, MockRumConfigElement*> none;
  MockAdminManager admin(none);
  NullLogger logger;

  const auto resolved = resolve(admin, config.get(), logger);
  ASSERT_TRUE(resolved.injects()) << "stable config alone must be enough to inject";
  const auto text = resolved.injected();
  EXPECT_NE(text.find("app-stable"), std::string::npos) << text;
  EXPECT_NE(text.find("tok-stable"), std::string::npos) << text;
  EXPECT_EQ(resolved.ctx().application_id_tag, "application_id:app-stable")
      << "the tag must come off the snippet, the only place stable-config values are visible";
}

TEST(StableConfig, IisOptionsOverrideStableConfigPerKey) {
  StableConfigFixture fixture("overlay");
  const auto local =
      fixture.write("local.yaml", browser_sdk_rule("    DD_RUM_APPLICATION_ID: app-stable\n"
                                                   "    DD_RUM_CLIENT_TOKEN: tok-stable\n"
                                                   "    DD_RUM_SERVICE: svc-stable\n"));
  auto config = fixture.read(local, fixture.absent("fleet.yaml"));
  ASSERT_EQ(config->error_code, 0) << config->error_message;

  // The section overrides service only; applicationId/clientToken stay.
  MockRumConfigElement section(true, {{L"service", L"svc-iis"}});
  MockAdminManager admin(&section);
  NullLogger logger;

  const auto resolved = resolve(admin, config.get(), logger);
  ASSERT_TRUE(resolved.injects());
  const auto text = resolved.injected();
  EXPECT_NE(text.find("svc-iis"), std::string::npos) << text;
  EXPECT_EQ(text.find("svc-stable"), std::string::npos) << "the IIS value must win";
  EXPECT_NE(text.find("app-stable"), std::string::npos)
      << "keys the section didn't mention must survive";
}

TEST(StableConfig, EmptyIisSectionDoesNotDefeatStableConfig) {
  // Risk: once the MSI registers datadogRum in configSections, IIS materialises
  // the section for every site. A schema-defaulted section must not turn into an
  // overlay, or it would pin majorVersion on sites nobody configured.
  StableConfigFixture fixture("empty-section");
  const auto local =
      fixture.write("local.yaml", browser_sdk_rule("    DD_RUM_APPLICATION_ID: app-stable\n"
                                                   "    DD_RUM_CLIENT_TOKEN: tok-stable\n"
                                                   "    DD_RUM_MAJOR_VERSION: 5\n"));
  auto config = fixture.read(local, fixture.absent("fleet.yaml"));
  ASSERT_EQ(config->error_code, 0) << config->error_message;

  MockRumConfigElement section(true, {}, 6);  // enabled, schema-defaulted version, no options
  section.SetVersionDefault(true);
  MockAdminManager admin(&section);
  NullLogger logger;

  const auto resolved = resolve(admin, config.get(), logger);
  ASSERT_TRUE(resolved.injects()) << "an empty section must be treated as 'unconfigured'";
  const auto text = resolved.injected();
  EXPECT_NE(text.find("app-stable"), std::string::npos) << text;
  EXPECT_NE(text.find("/v5/"), std::string::npos)
      << "majorVersion must come from stable config, not the section's schema default:\n"
      << text;
}

TEST(StableConfig, EmptyIisSectionPreservesInjectionStrategy) {
  StableConfigFixture fixture("strategy-only");
  const auto local =
      fixture.write("local.yaml", browser_sdk_rule("    DD_RUM_APPLICATION_ID: app-stable\n"
                                                   "    DD_RUM_CLIENT_TOKEN: tok-stable\n"));
  auto config = fixture.read(local, fixture.absent("fleet.yaml"));
  ASSERT_EQ(config->error_code, 0) << config->error_message;
  MockRumConfigElement section(true, {}, 6);
  section.SetVersionDefault(true);
  section.SetInjectionStrategy(InjectionStrategy::HeadOpenTag);
  MockAdminManager admin(&section);
  NullLogger logger;
  const auto resolved = resolve(admin, config.get(), logger);
  ASSERT_TRUE(resolved.injects());
  EXPECT_EQ(resolved.ctx().injection_strategy, InjectionStrategy::HeadOpenTag);
}

TEST(StableConfig, SchemaVersionDoesNotOverrideStableMajorWithIisOptions) {
  StableConfigFixture fixture("schema-version-overlay");
  const auto local =
      fixture.write("local.yaml", browser_sdk_rule("    DD_RUM_APPLICATION_ID: app-stable\n"
                                                   "    DD_RUM_CLIENT_TOKEN: tok-stable\n"
                                                   "    DD_RUM_MAJOR_VERSION: 5\n"));
  auto config = fixture.read(local, fixture.absent("fleet.yaml"));
  ASSERT_EQ(config->error_code, 0) << config->error_message;
  MockRumConfigElement section(true, {{L"service", L"svc-iis"}}, 6);
  section.SetVersionDefault(true);
  MockAdminManager admin(&section);
  NullLogger logger;
  const auto resolved = resolve(admin, config.get(), logger);
  ASSERT_TRUE(resolved.injects());
  EXPECT_NE(resolved.injected().find("/v5/"), std::string::npos);
  EXPECT_NE(resolved.injected().find("svc-iis"), std::string::npos);
}

TEST(StableConfig, ExplicitVersionWorksWithoutIisSdkOptions) {
  StableConfigFixture fixture("version-only");
  const auto local =
      fixture.write("local.yaml", browser_sdk_rule("    DD_RUM_APPLICATION_ID: app-stable\n"
                                                   "    DD_RUM_CLIENT_TOKEN: tok-stable\n"
                                                   "    DD_RUM_MAJOR_VERSION: 5\n"));
  auto config = fixture.read(local, fixture.absent("fleet.yaml"));
  ASSERT_EQ(config->error_code, 0) << config->error_message;
  MockRumConfigElement section(true, {}, 6);
  MockAdminManager admin(&section);
  NullLogger logger;
  const auto resolved = resolve(admin, config.get(), logger);
  ASSERT_TRUE(resolved.injects());
  EXPECT_NE(resolved.injected().find("/v6/"), std::string::npos);
}

TEST(StableConfig, DefaultMajorVersionMatchesTheIisSchema) {
  // The zero-touch case: the agent installer writes no DD_RUM_MAJOR_VERSION, so
  // the library default decides. It must match RUM_schema.xml's default of 6, or
  // two sites on one host would get different Browser SDK majors depending only
  // on whether they have a <datadogRum> section.
  StableConfigFixture fixture("default-major-version");
  const auto local =
      fixture.write("local.yaml", browser_sdk_rule("    DD_RUM_APPLICATION_ID: app-stable\n"
                                                   "    DD_RUM_CLIENT_TOKEN: tok-stable\n"));
  auto config = fixture.read(local, fixture.absent("fleet.yaml"));
  ASSERT_EQ(config->error_code, 0) << config->error_message;

  const std::map<std::wstring, MockRumConfigElement*> none;
  MockAdminManager admin(none);
  NullLogger logger;

  const auto resolved = resolve(admin, config.get(), logger);
  ASSERT_TRUE(resolved.injects());
  EXPECT_NE(resolved.injected().find("/v6/"), std::string::npos)
      << "expected the schema-aligned default of 6:\n"
      << resolved.injected();
}

TEST(StableConfig, MajorVersionComesFromTheSectionWhenItHasOne) {
  StableConfigFixture fixture("major-version-iis");
  const auto local =
      fixture.write("local.yaml", browser_sdk_rule("    DD_RUM_APPLICATION_ID: app-stable\n"
                                                   "    DD_RUM_CLIENT_TOKEN: tok-stable\n"
                                                   "    DD_RUM_MAJOR_VERSION: 5\n"));
  auto config = fixture.read(local, fixture.absent("fleet.yaml"));
  ASSERT_EQ(config->error_code, 0) << config->error_message;

  MockRumConfigElement section(true, {{L"service", L"svc-iis"}}, /* version */ 6);
  MockAdminManager admin(&section);
  NullLogger logger;

  const auto resolved = resolve(admin, config.get(), logger);
  ASSERT_TRUE(resolved.injects());
  EXPECT_NE(resolved.injected().find("/v6/"), std::string::npos)
      << "a real section keeps its own version, as it does today:\n"
      << resolved.injected();
}

TEST(StableConfig, DdRumEnabledFalseClearsInjection) {
  StableConfigFixture fixture("disabled-by-stable");
  const auto local =
      fixture.write("local.yaml", browser_sdk_rule("    DD_RUM_ENABLED: \"false\"\n"
                                                   "    DD_RUM_APPLICATION_ID: app-stable\n"
                                                   "    DD_RUM_CLIENT_TOKEN: tok-stable\n"));
  auto config = fixture.read(local, fixture.absent("fleet.yaml"));
  ASSERT_EQ(config->error_code, 0) << config->error_message;

  // A site that does configure itself through IIS is still switched off: the
  // section deliberately never emits `enabled`, so the disk value stands.
  MockRumConfigElement section(true, {{L"service", L"svc-iis"}});
  MockAdminManager admin(&section);
  NullLogger logger;

  const auto resolved = resolve(admin, config.get(), logger);
  EXPECT_FALSE(resolved.injects()) << "DD_RUM_ENABLED=false must switch injection off";
  EXPECT_TRUE(resolved.ctx().application_id_tag.empty());
}

TEST(StableConfig, IisEnabledFalseWinsOverStableConfig) {
  StableConfigFixture fixture("disabled-by-iis");
  const auto local =
      fixture.write("local.yaml", browser_sdk_rule("    DD_RUM_APPLICATION_ID: app-stable\n"
                                                   "    DD_RUM_CLIENT_TOKEN: tok-stable\n"));
  auto config = fixture.read(local, fixture.absent("fleet.yaml"));
  ASSERT_EQ(config->error_code, 0) << config->error_message;

  MockRumConfigElement section(false, {{L"service", L"svc-iis"}});
  MockAdminManager admin(&section);
  NullLogger logger;

  const auto resolved = resolve(admin, config.get(), logger);
  EXPECT_FALSE(resolved.injects()) << "an explicit per-site opt-out outranks stable config";
}

TEST(StableConfig, NothingConfiguredAnywhereIsQuiet) {
  StableConfigFixture fixture("nothing");
  auto config = fixture.read(fixture.absent("local.yaml"), fixture.absent("fleet.yaml"));
  ASSERT_NE(config, nullptr);
  EXPECT_EQ(config->error_code, 0) << "absent files are not an error";
  EXPECT_TRUE(config->is_empty);

  const std::map<std::wstring, MockRumConfigElement*> none;
  MockAdminManager admin(none);
  NullLogger logger;

  const auto resolved = resolve(admin, config.get(), logger);
  EXPECT_FALSE(resolved.injects());
  EXPECT_TRUE(resolved.ctx().application_id_tag.empty());
  EXPECT_TRUE(resolved.ctx().remote_config_tag.empty())
      << "the ordinary unconfigured host must not look like a misconfiguration";
}

TEST(StableConfig, BrokenYamlFallsBackToIisConfig) {
  // Highest-severity regression guard: one malformed application_monitoring.yaml
  // must not take injection down on sites configured entirely through IIS. The
  // handle is dropped at load time, so update_module_context sees nullptr.
  StableConfigFixture fixture("broken");
  const auto local = fixture.write("local.yaml", "::: not : yaml :::\n");
  auto config = fixture.read(local, fixture.absent("fleet.yaml"));
  ASSERT_NE(config, nullptr);
  EXPECT_NE(config->error_code, 0) << "a broken file must surface as a read error";

  MockRumConfigElement section(
      true,
      {{L"applicationId", L"app-iis"}, {L"clientToken", L"tok-iis"}, {L"site", L"datadoghq.com"}});
  MockAdminManager admin(&section);
  NullLogger logger;

  // GlobalModule::load_stable_config drops an unusable handle; model that here.
  const auto resolved = resolve(admin, /* stable_config */ nullptr, logger);
  ASSERT_TRUE(resolved.injects()) << "IIS-configured sites must keep injecting";
  EXPECT_NE(resolved.injected().find("app-iis"), std::string::npos);
  EXPECT_EQ(resolved.ctx().application_id_tag, "application_id:app-iis")
      << "the legacy path must still produce telemetry tags";
}

TEST(StableConfig, ReloadReplacesTheSnippetWithoutLeaking) {
  StableConfigFixture fixture("reload");
  const auto local =
      fixture.write("local.yaml", browser_sdk_rule("    DD_RUM_APPLICATION_ID: app-stable\n"
                                                   "    DD_RUM_CLIENT_TOKEN: tok-stable\n"));
  auto config = fixture.read(local, fixture.absent("fleet.yaml"));
  ASSERT_EQ(config->error_code, 0) << config->error_message;

  MockRumConfigElement first(true, {{L"service", L"svc-one"}});
  MockAdminManager admin(&first);
  NullLogger logger;
  MockServer server;
  EXPECT_CALL(server, GetAdminManager()).WillRepeatedly(Return(&admin));

  ModuleContext ctx;
  ctx.logger = &logger;
  ctx.server = &server;
  ctx.stable_config = config.get();

  update_module_context(ctx, server, k_site_path, logger, config.get());
  ASSERT_NE(ctx.snippet, nullptr);
  EXPECT_NE(inject(ctx.snippet).find("svc-one"), std::string::npos);

  // Second pass with different options: ASAN/leak checks in CI prove the old
  // snippet was released rather than overwritten.
  MockRumConfigElement second(true, {{L"service", L"svc-two"}});
  admin.SetConfig(&second);
  update_module_context(ctx, server, k_site_path, logger, config.get());
  ASSERT_NE(ctx.snippet, nullptr);
  const std::string text = inject(ctx.snippet);
  EXPECT_NE(text.find("svc-two"), std::string::npos) << text;
  EXPECT_EQ(text.find("svc-one"), std::string::npos);
}

TEST(StableConfig, FleetFileOverridesTheLocalOne) {
  StableConfigFixture fixture("fleet-precedence");
  const auto local =
      fixture.write("local.yaml", browser_sdk_rule("    DD_RUM_APPLICATION_ID: app-local\n"
                                                   "    DD_RUM_CLIENT_TOKEN: tok-local\n"));
  const auto fleet =
      fixture.write("fleet.yaml", browser_sdk_rule("    DD_RUM_APPLICATION_ID: app-fleet\n"
                                                   "    DD_RUM_CLIENT_TOKEN: tok-fleet\n"));
  auto config = fixture.read(local, fleet);
  ASSERT_EQ(config->error_code, 0) << config->error_message;

  const std::map<std::wstring, MockRumConfigElement*> none;
  MockAdminManager admin(none);
  NullLogger logger;

  const auto resolved = resolve(admin, config.get(), logger);
  ASSERT_TRUE(resolved.injects());
  EXPECT_EQ(resolved.ctx().application_id_tag, "application_id:app-fleet");
}

TEST(StableConfig, FailedIisReloadPreservesExplicitOptOut) {
  StableConfigFixture fixture("failed-reload");
  const auto local =
      fixture.write("local.yaml", browser_sdk_rule("    DD_RUM_APPLICATION_ID: app-stable\n"
                                                   "    DD_RUM_CLIENT_TOKEN: tok-stable\n"));
  auto config = fixture.read(local, fixture.absent("fleet.yaml"));
  ASSERT_EQ(config->error_code, 0) << config->error_message;
  MockRumConfigElement section(false, {});
  MockAdminManager admin(&section);
  MockServer server;
  EXPECT_CALL(server, GetAdminManager()).WillRepeatedly(Return(&admin));
  NullLogger logger;
  ModuleContext ctx;
  update_module_context(ctx, server, k_site_path, logger, config.get());
  ASSERT_EQ(ctx.snippet, nullptr);
  for (HRESULT error : {E_FAIL, E_ACCESSDENIED}) {
    admin.SetSectionResult(error);
    update_module_context(ctx, server, k_site_path, logger, config.get());
    EXPECT_EQ(ctx.snippet, nullptr);
  }
}

TEST(StableConfig, RemovingIisOptionsWithNoStableRumClearsInjection) {
  StableConfigFixture fixture("removed-iis-options");
  auto config = fixture.read(fixture.absent("local.yaml"), fixture.absent("fleet.yaml"));
  ASSERT_EQ(config->error_code, 0) << config->error_message;
  MockRumConfigElement enabled;
  MockRumConfigElement empty(true, {}, 6);
  empty.SetVersionDefault(true);
  MockAdminManager admin(&enabled);
  MockServer server;
  EXPECT_CALL(server, GetAdminManager()).WillRepeatedly(Return(&admin));
  NullLogger logger;
  ModuleContext ctx;
  update_module_context(ctx, server, k_site_path, logger, config.get());
  ASSERT_NE(ctx.snippet, nullptr);
  admin.SetConfig(&empty);
  update_module_context(ctx, server, k_site_path, logger, config.get());
  EXPECT_EQ(ctx.snippet, nullptr);
}

TEST(StableConfig, InvalidStableValuesFallBackToCompleteIisConfig) {
  const std::vector<std::string> invalid_values = {
      "    DD_RUM_MAJOR_VERSION: nope\n",       "    DD_RUM_MAJOR_VERSION: 99\n",
      "    DD_RUM_SESSION_SAMPLE_RATE: nope\n", "    DD_RUM_SESSION_SAMPLE_RATE: 101\n",
      "    DD_RUM_TRACK_RESOURCES: nope\n",     "    DD_RUM_SITE: invalid.example\n"};
  for (const auto& invalid : invalid_values) {
    SCOPED_TRACE(invalid);
    StableConfigFixture fixture("invalid-stable-value");
    const auto local =
        fixture.write("local.yaml", browser_sdk_rule("    DD_RUM_APPLICATION_ID: app-stable\n"
                                                     "    DD_RUM_CLIENT_TOKEN: tok-stable\n" +
                                                     invalid));
    auto config = fixture.read(local, fixture.absent("fleet.yaml"));
    ASSERT_EQ(config->error_code, 0) << config->error_message;
    MockRumConfigElement section(
        true, {{L"applicationId", L"test-app-id"}, {L"clientToken", L"test-client-token"}}, 6);
    section.SetVersionDefault(true);
    MockAdminManager admin(&section);
    NullLogger logger;
    const auto resolved = resolve(admin, config.get(), logger);
    ASSERT_TRUE(resolved.injects());
    EXPECT_NE(resolved.injected().find("test-app-id"), std::string::npos);
    EXPECT_EQ(resolved.injected().find("app-stable"), std::string::npos);
  }
}

TEST(StableConfig, IisOverridesInvalidValuesWithoutDroppingStableCredentials) {
  StableConfigFixture fixture("invalid-overridden-stable-values");
  const auto local = fixture.write(
      "local.yaml", browser_sdk_rule("    DD_RUM_APPLICATION_ID: app-stable\n"
                                     "    DD_RUM_CLIENT_TOKEN: tok-stable\n"
                                     "    DD_RUM_SERVICE: svc-stable\n"
                                     "    DD_RUM_MAJOR_VERSION: nope\n"
                                     "    DD_RUM_TRACK_USER_INTERACTIONS: nope\n"
                                     "    DD_RUM_TRACK_RESOURCES: nope\n"
                                     "    DD_RUM_TRACK_LONG_TASKS: nope\n"
                                     "    DD_RUM_SESSION_SAMPLE_RATE: nope\n"
                                     "    DD_RUM_SESSION_REPLAY_SAMPLE_RATE: nope\n"));
  auto config = fixture.read(local, fixture.absent("fleet.yaml"));
  ASSERT_EQ(config->error_code, 0) << config->error_message;
  MockRumConfigElement section(true,
                               {{L"trackUserInteractions", L"false"},
                                {L"trackResources", L"false"},
                                {L"trackLongTasks", L"false"},
                                {L"sessionSampleRate", L"0"},
                                {L"sessionReplaySampleRate", L"0"}},
                               6);
  MockAdminManager admin(&section);
  NullLogger logger;
  const auto resolved = resolve(admin, config.get(), logger);
  ASSERT_TRUE(resolved.injects());
  EXPECT_NE(resolved.injected().find("app-stable"), std::string::npos);
  EXPECT_NE(resolved.injected().find("tok-stable"), std::string::npos);
  EXPECT_NE(resolved.injected().find("svc-stable"), std::string::npos);
  EXPECT_NE(resolved.injected().find("/v6/"), std::string::npos);
}

TEST(StableConfig, StableOptOutWinsOverInvalidValuesAndCompleteIisConfig) {
  StableConfigFixture fixture("disabled-invalid-stable");
  const auto local =
      fixture.write("local.yaml", browser_sdk_rule("    DD_RUM_ENABLED: \"false\"\n"
                                                   "    DD_RUM_SESSION_SAMPLE_RATE: nope\n"));
  auto config = fixture.read(local, fixture.absent("fleet.yaml"));
  ASSERT_EQ(config->error_code, 0) << config->error_message;
  MockRumConfigElement section;
  MockAdminManager admin(&section);
  NullLogger logger;
  const auto resolved = resolve(admin, config.get(), logger);
  EXPECT_FALSE(resolved.injects());
}
