// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2024-Present Datadog, Inc.

#include "telemetry.h"

#include <atomic>
#include <datadog/telemetry/telemetry.h>
#include <vector>

using namespace datadog::telemetry;

namespace datadog::rum::telemetry {

const Counter injection_skipped = {"injection.skipped", "rum", true};
const Counter injection_succeed = {"injection.succeed", "rum", true};
const Counter injection_failed = {"injection.failed", "rum", true};
const Counter config_invalid = {"config.invalid", "rum", true};

namespace {
// Written once from GlobalModule's constructor (inside RegisterModule, before
// any notification is registered) and read from worker threads afterwards.
std::atomic<bool> g_ready{false};
}  // namespace

void mark_ready() { g_ready.store(true, std::memory_order_release); }

void increment(const Counter& counter, const std::vector<std::string>& tags) {
  if (!g_ready.load(std::memory_order_acquire)) {
    return;
  }
  ::datadog::telemetry::counter::increment(counter, tags);
}

}  // namespace datadog::rum::telemetry
