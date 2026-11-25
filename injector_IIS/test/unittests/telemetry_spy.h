// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2025-Present Datadog, Inc.

// Reads back what the module counts with telemetry. The telemetry module is a process-wide
// singleton that only its first `init` sets up: the environment below does it before any test,
// with an HTTP client that keeps what it sends and a scheduler that runs its tasks on demand.

#pragma once

#include "gtest/gtest.h"
#include <datadog/event_scheduler.h>
#include <datadog/http_client.h>
#include <datadog/logger.h>
#include <datadog/telemetry/configuration.h>
#include <datadog/telemetry/telemetry.h>
#include <rapidjson/document.h>

#include <algorithm>
#include <chrono>
#include <functional>
#include <map>
#include <memory>
#include <string>
#include <string_view>
#include <vector>

namespace telemetry_spy {

class NullLogger : public datadog::tracing::Logger {
 public:
  using datadog::tracing::Logger::log_error;
  void log_error(const LogFunc&) override {}
  void log_startup(const LogFunc&) override {}
};

class RecordingClient : public datadog::tracing::HTTPClient {
 public:
  std::vector<std::string> bodies;

  datadog::tracing::Expected<void> post(const URL&, HeadersSetter, std::string body,
                                        ResponseHandler, ErrorHandler,
                                        std::chrono::steady_clock::time_point) override {
    bodies.push_back(std::move(body));
    return {};
  }
  void drain(std::chrono::steady_clock::time_point) override {}
  std::string config() const override { return "{}"; }
};

class ManualScheduler : public datadog::tracing::EventScheduler {
 public:
  std::map<std::chrono::steady_clock::duration, std::function<void()>> tasks;

  Cancel schedule_recurring_event(std::chrono::steady_clock::duration interval,
                                  std::function<void()> callback) override {
    tasks[interval] = std::move(callback);
    return [] {};
  }
  std::string config() const override { return "{}"; }
};

// One injection count: the counter, and the tags it was counted with.
struct Count {
  std::string metric;
  std::vector<std::string> tags;

  // The counter and its reason tag, if any: "injection.failed reason:missing_header_tag".
  std::string describe() const {
    std::string description = metric;
    for (const std::string& tag : tags) {
      if (tag.starts_with("reason:")) {
        description += " " + tag;
      }
    }
    return description;
  }

  bool has_tag_starting_with(std::string_view prefix) const {
    return std::any_of(tags.begin(), tags.end(),
                       [prefix](const std::string& tag) { return tag.starts_with(prefix); });
  }
};

class Environment : public testing::Environment {
 public:
  void SetUp() override {
    datadog::telemetry::Configuration config;
    config.enabled = true;
    config.integration_name = "iis";
    auto finalized = datadog::telemetry::finalize_config(config);
    ASSERT_EQ(finalized.if_error(), nullptr);
    metrics_interval_ = finalized->metrics_interval;
    heartbeat_interval_ = finalized->heartbeat_interval;
    datadog::telemetry::init(*finalized, std::make_shared<NullLogger>(), client_, scheduler_,
                             datadog::tracing::HTTPClient::URL{"http", "localhost:8126", "", ""});
  }

  // Returns the injection counts since the last call, one per increment.
  std::vector<Count> take() {
    client_->bodies.clear();
    scheduler_->tasks.at(metrics_interval_)();
    scheduler_->tasks.at(heartbeat_interval_)();

    std::vector<Count> counts;
    for (const std::string& body : client_->bodies) {
      rapidjson::Document batch;
      batch.Parse(body.c_str());
      if (batch.HasParseError() || !batch.HasMember("payload") || !batch["payload"].IsArray()) {
        continue;
      }
      for (const auto& message : batch["payload"].GetArray()) {
        if (!message.HasMember("request_type") ||
            std::string_view(message["request_type"].GetString()) != "generate-metrics") {
          continue;
        }
        for (const auto& series : message["payload"]["series"].GetArray()) {
          Count count{series["metric"].GetString(), {}};
          if (!count.metric.starts_with("injection.")) {
            continue;
          }
          if (series.HasMember("tags")) {
            for (const auto& tag : series["tags"].GetArray()) {
              count.tags.emplace_back(tag.GetString());
            }
          }
          for (const auto& point : series["points"].GetArray()) {
            counts.insert(counts.end(), point[1].GetUint64(), count);
          }
        }
      }
    }
    return counts;
  }

 private:
  std::shared_ptr<RecordingClient> client_ = std::make_shared<RecordingClient>();
  std::shared_ptr<ManualScheduler> scheduler_ = std::make_shared<ManualScheduler>();
  std::chrono::steady_clock::duration metrics_interval_{};
  std::chrono::steady_clock::duration heartbeat_interval_{};
};

inline Environment* const environment =
    static_cast<Environment*>(testing::AddGlobalTestEnvironment(new Environment));

// Drops the injection counts so far.
inline void reset() { environment->take(); }

// Returns the injection counts since the last call, described, in order. Each must carry the
// application id and remote config tags.
inline std::vector<std::string> take_counts() {
  std::vector<std::string> described;
  for (const Count& count : environment->take()) {
    EXPECT_TRUE(count.has_tag_starting_with("application_id:")) << count.describe();
    EXPECT_TRUE(count.has_tag_starting_with("remote_config_used:")) << count.describe();
    described.push_back(count.describe());
  }
  std::sort(described.begin(), described.end());
  return described;
}

}  // namespace telemetry_spy
