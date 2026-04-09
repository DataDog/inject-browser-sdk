// IIS interface definitions extracted verbatim from microsoft/IIS.Common.
// Source:
// https://github.com/microsoft/IIS.Common/blob/main/Include/httpserv_xp.h
// License: MIT — see IIS.Common.LICENSE in this directory.
//
// This file is a reference copy of the IIS interfaces we mock in mocks.h.
// It is NOT included by any build target — it exists solely so that reviewers
// (human or AI) can verify mock signatures against the real SDK without
// needing a Windows SDK installation.
//
// When adding or updating mocks, diff the relevant class below against the
// mock in mocks.h to ensure every pure-virtual method is covered and
// signatures match.

#pragma once

// ---- IHttpEventProvider -------------------------------------------------

class IHttpEventProvider {
 public:
  virtual VOID SetErrorStatus(HRESULT hrError) = 0;
};

// ---- IHttpStoredContext --------------------------------------------------

class IHttpStoredContext {
 public:
  virtual VOID CleanupStoredContext(VOID) = 0;
};

// ---- IHttpModuleContextContainer ----------------------------------------

class IHttpModuleContextContainer {
 public:
  virtual IHttpStoredContext* GetModuleContext(IN HTTP_MODULE_ID moduleId) = 0;

  virtual HRESULT SetModuleContext(IN IHttpStoredContext* ppStoredContext,
                                   IN HTTP_MODULE_ID moduleId) = 0;
};

// ---- IHttpRequest -------------------------------------------------------

class IHttpRequest {
 public:
  virtual HTTP_REQUEST* GetRawHttpRequest(VOID) = 0;

  virtual const HTTP_REQUEST* GetRawHttpRequest(VOID) const = 0;

  virtual PCSTR GetHeader(IN PCSTR pszHeaderName,
                          OUT USHORT* pcchHeaderValue = NULL) const = 0;

  virtual PCSTR GetHeader(IN HTTP_HEADER_ID ulHeaderIndex,
                          OUT USHORT* pcchHeaderValue = NULL) const = 0;

  virtual HRESULT SetHeader(IN PCSTR pszHeaderName, IN PCSTR pszHeaderValue,
                            IN USHORT cchHeaderValue, IN BOOL fReplace) = 0;

  virtual HRESULT SetHeader(IN HTTP_HEADER_ID ulHeaderIndex,
                            IN PCSTR pszHeaderValue, IN USHORT cchHeaderValue,
                            IN BOOL fReplace) = 0;

  virtual HRESULT DeleteHeader(IN PCSTR pszHeaderName) = 0;

  virtual HRESULT DeleteHeader(IN HTTP_HEADER_ID ulHeaderIndex) = 0;

  virtual PCSTR GetHttpMethod(VOID) const = 0;

  virtual HRESULT SetHttpMethod(IN PCSTR pszHttpMethod) = 0;

  virtual HRESULT SetUrl(IN PCWSTR pszUrl, IN DWORD cchUrl,
                         IN BOOL fResetQueryString) = 0;

  virtual HRESULT SetUrl(IN PCSTR pszUrl, IN DWORD cchUrl,
                         IN BOOL fResetQueryString) = 0;

  virtual BOOL GetUrlChanged(VOID) const = 0;

  virtual PCWSTR GetForwardedUrl(VOID) const = 0;

  virtual PSOCKADDR GetLocalAddress(VOID) const = 0;

  virtual PSOCKADDR GetRemoteAddress(VOID) const = 0;

  virtual HRESULT ReadEntityBody(OUT VOID* pvBuffer, IN DWORD cbBuffer,
                                 IN BOOL fAsync, OUT DWORD* pcbBytesReceived,
                                 OUT BOOL* pfCompletionPending = NULL) = 0;

  virtual HRESULT InsertEntityBody(IN VOID* pvBuffer, IN DWORD cbBuffer) = 0;

  virtual DWORD GetRemainingEntityBytes(VOID) = 0;

  virtual VOID GetHttpVersion(OUT USHORT* pMajorVersion,
                              OUT USHORT* pMinorVersion) const = 0;

  virtual HRESULT GetClientCertificate(
      OUT HTTP_SSL_CLIENT_CERT_INFO** ppClientCertInfo,
      OUT BOOL* pfClientCertNegotiated) = 0;

  virtual HRESULT NegotiateClientCertificate(
      IN BOOL fAsync, OUT BOOL* pfCompletionPending = NULL) = 0;

  virtual DWORD GetSiteId(VOID) const = 0;

  virtual HRESULT GetHeaderChanges(
      IN DWORD dwOldChangeNumber, OUT DWORD* pdwNewChangeNumber,
      IN OUT PCSTR knownHeaderSnapshot[HttpHeaderRequestMaximum],
      IN OUT DWORD* pdwUnknownHeaderSnapshot,
      IN OUT PCSTR** ppUnknownHeaderNameSnapshot,
      IN OUT PCSTR** ppUnknownHeaderValueSnapshot,
      __out_ecount(HttpHeaderRequestMaximum + 1)
          DWORD diffedKnownHeaderIndices[HttpHeaderRequestMaximum + 1],
      OUT DWORD* pdwDiffedUnknownHeaders,
      OUT DWORD** ppDiffedUnknownHeaderIndices) = 0;
};
