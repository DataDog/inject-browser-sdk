/*
 * Unless explicitly stated otherwise all files in this repository are licensed
 * under the Apache 2.0 License. This product includes software developed at
 * Datadog (https://www.datadoghq.com/).
 *
 * Copyright 2024-Present Datadog, Inc.
 */

#pragma once

#include "framework.h"
#include "module_context.h"
#include <new>

namespace datadog::rum {

constexpr const char* k_injection_header = "x-datadog-rum-injected";
constexpr const char* k_injection_pending_header = "x-datadog-rum-injection-pending";

class HttpModule : public CHttpModule {
 public:
  static const DWORD NotificationsMask = RQ_SEND_RESPONSE | RQ_BEGIN_REQUEST;

  REQUEST_NOTIFICATION_STATUS OnBeginRequest(IN IHttpContext* pHttpContext,
                                             IN IHttpEventProvider* pProvider) override;

  REQUEST_NOTIFICATION_STATUS
  OnSendResponse(IN IHttpContext* pHttpContext, IN ISendResponseProvider* pProvider) override;

  bool ShouldAttemptInjection(IHttpResponse& pHttpResponse, const ModuleContext& ctx) const;

  // Where a send carrying the start of the body stands in the response, see OnSendResponse.
  struct Send {
    // More sends of the response follow this one.
    bool more_data;
    // The headers went out with an earlier send, which carried no body: they can no longer change.
    bool headers_gone;
  };

  REQUEST_NOTIFICATION_STATUS PerformInjection(IHttpContext& pHttpContext,
                                               IHttpResponse& pHttpResponse, ModuleContext& ctx,
                                               Send send);
};

class HttpModuleFactory : public IHttpModuleFactory {
 public:
  ~HttpModuleFactory() = default;

  HRESULT GetHttpModule(OUT CHttpModule** ppModule, IN IModuleAllocator*) override {
    auto* http_module = new (std::nothrow) HttpModule;
    if (http_module == nullptr) {
      return E_OUTOFMEMORY;
    }

    *ppModule = http_module;
    return S_OK;
  }

  inline void Terminate() override { delete this; }
};

}  // namespace datadog::rum
