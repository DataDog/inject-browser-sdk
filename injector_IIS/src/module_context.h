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
  Snippet* snippet = nullptr;
  Logger* logger = nullptr;
  IHttpServer* server = nullptr;
  std::string application_id_tag;
  std::string remote_config_tag;
  // Where the snippet is injected, resolved from the `datadogRum` section and applied when the
  // injector is created. The default matches `injector_create`'s.
  InjectionStrategy injection_strategy = InjectionStrategy::HeadCloseTag;
  // Whether a file chunk, a static file IIS serves from disk, is read for the injector to scan, or
  // ends the scan as a chunk that cannot be read does. From the `injectStaticFiles` attribute.
  bool inject_static_files = true;

  ~ModuleContext() {
    if (snippet != nullptr) {
      snippet_cleanup(snippet);
    }
  }

  // IIS calls this instead of the destructor (COM-style lifecycle).
  void CleanupStoredContext() override { delete this; };
};

}  // namespace datadog::rum
