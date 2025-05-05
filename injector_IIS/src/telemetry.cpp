// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2024-Present Datadog, Inc.

#include "telemetry.h"

using namespace datadog::telemetry;

namespace datadog::rum::telemetry {

std::shared_ptr<Counter> injection_skipped(new Counter("injection.skipped",
                                                       "rum", true));
std::shared_ptr<Counter> injection_succeed(new Counter("injection.succeed",
                                                       "rum", true));
std::shared_ptr<Counter> injection_failed(new Counter("injection.failed", "rum",
                                                      true));

} // namespace datadog::rum::telemetry
