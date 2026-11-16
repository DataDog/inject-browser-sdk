/*
 * Unless explicitly stated otherwise all files in this repository are licensed
 * under the Apache 2.0 License. This product includes software developed at
 * Datadog (https://www.datadoghq.com/).
 *
 * Copyright 2025-Present Datadog, Inc.
 */

#pragma once

#include "framework.h"
#include "iis_rum_runtime_abi.h"

extern "C" __declspec(dllexport) HRESULT __stdcall RegisterModule(
    DWORD dwServerVersion, IHttpModuleRegistrationInfo* pModuleInfo, IHttpServer* pGlobalInfo);

extern "C" __declspec(dllexport) HRESULT __stdcall DatadogRumCreateRuntimeV1(
    DWORD server_version, IHttpServer* server, HTTP_MODULE_ID bootstrap_module_id,
    DatadogRumRuntimeV1* runtime);
