// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2024-Present Datadog, Inc.

#include "telemetry.h"

using namespace datadog::telemetry;

namespace datadog::rum::telemetry {

// Counter holds std::strings, so these can only throw bad_alloc while the DLL loads, which nothing
// could recover from anyway.
// NOLINTBEGIN(bugprone-throwing-static-initialization)
const Counter injection_skipped = {"injection.skipped", "rum", true};
const Counter injection_succeed = {"injection.succeed", "rum", true};
const Counter injection_failed = {"injection.failed", "rum", true};
const Counter config_invalid = {"config.invalid", "rum", true};
// NOLINTEND(bugprone-throwing-static-initialization)

}  // namespace datadog::rum::telemetry
