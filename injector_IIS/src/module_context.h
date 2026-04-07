/*
 * Unless explicitly stated otherwise all files in this repository are licensed
 * under the Apache 2.0 License. This product includes software developed at
 * Datadog (https://www.datadoghq.com/).
 *
 * Copyright 2024-Present Datadog, Inc.
 */

#pragma once

#include "framework.h"
#include "injectbrowsersdk.h"
#include "logger.h"
#include <string>

namespace datadog::rum {

// Module identifier retrieve from the module info in register module function.
extern HTTP_MODULE_ID g_module_id;

struct ModuleContext final : public IHttpStoredContext {
  Snippet *snippet = nullptr;
  Logger *logger = nullptr;
  IHttpServer *server = nullptr;
  std::string application_id_tag;
  std::string remote_config_tag;

  // IIS calls this instead of the destructor (COM-style lifecycle).
  void CleanupStoredContext() override {
    if (snippet != nullptr) {
      snippet_cleanup(snippet);
    }
    delete this;
  };
};

} // namespace datadog::rum
