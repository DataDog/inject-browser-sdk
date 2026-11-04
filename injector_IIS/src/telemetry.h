/*
 * Unless explicitly stated otherwise all files in this repository are licensed
 * under the Apache 2.0 License. This product includes software developed at
 * Datadog (https://www.datadoghq.com/).
 *
 * Copyright 2024-Present Datadog, Inc.
 */

#pragma once

#include "version.h"
#include <datadog/telemetry/metrics.h>
#include <format>
#include <memory>
#include <string>
#include <utility>
#include <vector>

namespace datadog::rum::telemetry {

template <typename... T>
auto build_tags(T&&... specific_tags) {
  std::vector<std::string> tags = {std::forward<T>(specific_tags)...};
  tags.emplace_back("integration_name:iis");
  tags.emplace_back(
      std::format("injector_version:{}", std::string_view{datadog_rum_injector_version}));
  tags.emplace_back(std::format("integration_version:{}", SHORT_VERSION_STRING));

  return tags;
}

const extern datadog::telemetry::Counter injection_skipped;
const extern datadog::telemetry::Counter injection_succeed;
const extern datadog::telemetry::Counter injection_failed;
const extern datadog::telemetry::Counter config_invalid;

// Records that `datadog::telemetry::init` has completed. Call exactly once, and
// only after it returns.
void mark_ready();

// Increments `counter` if and only if the telemetry subsystem was initialised.
//
// Never call datadog::telemetry::counter::increment directly. dd-trace-cpp's
// telemetry instance is a function-local static built from an
// `Optional<Ctor_param>` that it dereferences unconditionally:
//
//     static TelemetryProxy telemetry(make_telemetry(*init));
//
// The static is constructed on the *first* call, so if that first call comes
// from increment() rather than from init(), it dereferences an empty Optional
// and takes down the worker process with an access violation. Two ways to reach
// that: emitting during GlobalModule's constructor before init runs, or emitting
// from a request after init was skipped because agent-config finalization
// failed.
void increment(const datadog::telemetry::Counter& counter, const std::vector<std::string>& tags);

}  // namespace datadog::rum::telemetry
