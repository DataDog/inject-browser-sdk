// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2025-Present Datadog, Inc.

#include "module_context.h"

#include <atomic>

namespace datadog::rum {

HTTP_MODULE_ID g_module_id;

namespace {
std::atomic_bool module_id_claimed{false};
}

bool try_claim_module_id(HTTP_MODULE_ID id) noexcept {
  bool expected = false;
  if (!module_id_claimed.compare_exchange_strong(expected, true, std::memory_order_acq_rel)) {
    return false;
  }
  g_module_id = id;
  return true;
}

void release_module_id_claim() noexcept {
  g_module_id = nullptr;
  module_id_claimed.store(false, std::memory_order_release);
}

}  // namespace datadog::rum
