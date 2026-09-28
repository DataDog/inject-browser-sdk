// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2026-Present Datadog, Inc.

#pragma once

#include <httpserv.h>

// This header is the single ABI definition for the bootstrap and optional runtime.
// clang-format off
// Shared by the durable IIS bootstrap and the optional RUM package. Keep this
// layout and calling convention unchanged for ABI version 1.
struct DatadogRumRuntimeV1 {
  DWORD size;
  DWORD version;
  void *runtime;
  CGlobalModule *global_module;
  IHttpModuleFactory *request_factory;
  HRESULT(__stdcall *prepare_app)(void *runtime, IHttpApplication *application);
};

using DatadogRumCreateRuntimeV1Fn = HRESULT(__stdcall *)(
    DWORD server_version,
    IHttpServer *server,
    HTTP_MODULE_ID bootstrap_module_id,
    DatadogRumRuntimeV1 *runtime
);

constexpr DWORD kDatadogRumRuntimeAbiVersion = 1;
// clang-format on
