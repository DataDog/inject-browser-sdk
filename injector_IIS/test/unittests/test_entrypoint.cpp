// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2024-Present Datadog, Inc.

#include "entrypoint.h"
#include "mocks.h"
#include "gtest/gtest.h"
#include <string>

namespace {
class StableConfigTestIsolation final : public testing::Environment {
 public:
  void SetUp() override {
    wchar_t temp_path[MAX_PATH];
    DWORD length = GetTempPathW(MAX_PATH, temp_path);
    ASSERT_GT(length, 0);
    ASSERT_LT(length, MAX_PATH);
    const std::wstring prefix = std::wstring(temp_path, length) + L"iis-rum-test-missing-" +
                                std::to_wstring(GetCurrentProcessId());
    ASSERT_NE(SetEnvironmentVariableW(L"DD_IIS_RUM_STABLE_CONFIG_LOCAL",
                                      (prefix + L"-local.yaml").c_str()),
              0);
    ASSERT_NE(SetEnvironmentVariableW(L"DD_IIS_RUM_STABLE_CONFIG_FLEET",
                                      (prefix + L"-fleet.yaml").c_str()),
              0);
  }
};

[[maybe_unused]] testing::Environment* const stable_config_test_isolation =
    testing::AddGlobalTestEnvironment(new StableConfigTestIsolation());
}  // namespace

using ::testing::_;  // NOLINT(bugprone-reserved-identifier) gtest idiom
using ::testing::DoAll;
using ::testing::Return;
using ::testing::SaveArg;

TEST(TestRegisterModule, TestRegistersForBeginRequestAndSendResponseNotifications) {
  MockModule module;
  MockServer server;

  EXPECT_CALL(server, GetAdminManager()).WillRepeatedly(Return(nullptr));

  EXPECT_CALL(module, SetGlobalNotifications(
                          _, GL_APPLICATION_START | GL_CONFIGURATION_CHANGE | GL_APPLICATION_STOP))
      .Times(1);
  EXPECT_CALL(module, SetRequestNotifications(_, RQ_SEND_RESPONSE | RQ_BEGIN_REQUEST, _)).Times(1);
  EXPECT_CALL(module, GetId()).Times(1);

  auto result = RegisterModule(0, &module, &server);

  EXPECT_EQ(result, S_OK);
}

TEST(TestRegisterModule, TestRegistersValidHTTPModuleFactory) {
  MockModule module;
  MockServer server;

  EXPECT_CALL(server, GetAdminManager()).WillRepeatedly(Return(nullptr));

  IHttpModuleFactory* resultFactory = nullptr;
  ON_CALL(module, SetRequestNotifications)
      .WillByDefault(DoAll(SaveArg<0>(&resultFactory), Return(S_OK)));

  EXPECT_CALL(module, SetRequestNotifications).Times(1);
  EXPECT_CALL(module, SetGlobalNotifications).Times(1);
  EXPECT_CALL(module, GetId).Times(1);

  auto result = RegisterModule(0, &module, &server);

  EXPECT_EQ(result, S_OK);
  ASSERT_NE(resultFactory, nullptr);

  CHttpModule* resultFactoryOutput = nullptr;
  result = resultFactory->GetHttpModule(&resultFactoryOutput, nullptr);

  EXPECT_EQ(result, S_OK);
  EXPECT_NE(resultFactoryOutput, nullptr);

  resultFactory->Terminate();
}

TEST(TestRegisterModule, BootstrapRuntimeExportReturnsOwnedObjects) {
  MockServer server;
  EXPECT_CALL(server, GetAdminManager()).WillRepeatedly(Return(nullptr));

  DatadogRumRuntimeV1 runtime{};
  runtime.size = sizeof(runtime);
  runtime.version = kDatadogRumRuntimeAbiVersion;
  ASSERT_EQ(DatadogRumCreateRuntimeV1(0, &server, nullptr, &runtime), S_OK);
  EXPECT_EQ(runtime.version, kDatadogRumRuntimeAbiVersion);
  EXPECT_EQ(runtime.size, sizeof(runtime));
  ASSERT_NE(runtime.runtime, nullptr);
  ASSERT_NE(runtime.global_module, nullptr);
  ASSERT_NE(runtime.request_factory, nullptr);
  ASSERT_NE(runtime.prepare_app, nullptr);
  EXPECT_EQ(runtime.prepare_app(runtime.runtime, nullptr), E_POINTER);

  runtime.request_factory->Terminate();
  runtime.global_module->Terminate();
}

TEST(TestRegisterModule, BootstrapRuntimeExportChecksCallerBuffer) {
  MockServer server;
  DatadogRumRuntimeV1 runtime{};
  runtime.size = sizeof(runtime) - 1;
  runtime.version = kDatadogRumRuntimeAbiVersion;
  EXPECT_EQ(DatadogRumCreateRuntimeV1(0, &server, nullptr, &runtime), E_INVALIDARG);
  runtime.size = sizeof(runtime);
  runtime.version = 0;
  EXPECT_EQ(DatadogRumCreateRuntimeV1(0, &server, nullptr, &runtime), E_INVALIDARG);
  EXPECT_EQ(DatadogRumCreateRuntimeV1(0, &server, nullptr, nullptr), E_INVALIDARG);
}

TEST(TestRegisterModule, BootstrapRuntimeRejectsSecondModuleId) {
  MockServer server;
  EXPECT_CALL(server, GetAdminManager()).WillRepeatedly(Return(nullptr));
  DatadogRumRuntimeV1 runtime{};
  runtime.size = sizeof(runtime);
  runtime.version = kDatadogRumRuntimeAbiVersion;
  ASSERT_EQ(DatadogRumCreateRuntimeV1(0, &server, nullptr, &runtime), S_OK);

  MockModule duplicate;
  EXPECT_CALL(duplicate, GetId()).Times(1);
  EXPECT_CALL(duplicate, SetGlobalNotifications).Times(0);
  EXPECT_CALL(duplicate, SetRequestNotifications).Times(0);
  EXPECT_EQ(RegisterModule(0, &duplicate, &server), S_OK);

  runtime.request_factory->Terminate();
  runtime.global_module->Terminate();
}

TEST(TestRegisterModule, ManualRegistrationRejectsBootstrapRuntime) {
  MockServer server;
  EXPECT_CALL(server, GetAdminManager()).WillRepeatedly(Return(nullptr));
  MockModule module;
  EXPECT_CALL(module, GetId()).Times(1);
  CGlobalModule* global = nullptr;
  IHttpModuleFactory* factory = nullptr;
  EXPECT_CALL(module, SetGlobalNotifications).WillOnce(DoAll(SaveArg<0>(&global), Return(S_OK)));
  EXPECT_CALL(module, SetRequestNotifications).WillOnce(DoAll(SaveArg<0>(&factory), Return(S_OK)));
  ASSERT_EQ(RegisterModule(0, &module, &server), S_OK);

  DatadogRumRuntimeV1 runtime{};
  runtime.size = sizeof(runtime);
  runtime.version = kDatadogRumRuntimeAbiVersion;
  EXPECT_EQ(DatadogRumCreateRuntimeV1(0, &server, nullptr, &runtime),
            HRESULT_FROM_WIN32(ERROR_ALREADY_EXISTS));
  EXPECT_EQ(runtime.global_module, nullptr);
  EXPECT_EQ(runtime.request_factory, nullptr);

  ASSERT_NE(factory, nullptr);
  ASSERT_NE(global, nullptr);
  factory->Terminate();
  global->Terminate();
}
