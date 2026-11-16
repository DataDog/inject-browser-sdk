// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2025-Present Datadog, Inc.

#include "entrypoint.h"
#include "mocks.h"
#include "gtest/gtest.h"
#include <string>

namespace {
class StableConfigTestIsolation final : public testing::Environment {
 public:
  void SetUp() override {
    wchar_t temp_path[MAX_PATH];
    const DWORD length = GetTempPathW(MAX_PATH, temp_path);
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

// gtest registers environments this way; an allocation failure at startup aborts the run anyway.
// NOLINTNEXTLINE(bugprone-throwing-static-initialization)
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

  const auto result = RegisterModule(0, &module, &server);

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
