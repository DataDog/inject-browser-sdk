# IIS Unit Tests

## Mocking IIS interfaces

The IIS interfaces (`IHttpRequest`, `IHttpResponse`, `IHttpEventProvider`, `IHttpContext`, etc.) are defined in the Windows SDK header `<httpserv.h>`. This header is not available locally on macOS/Linux — it's pulled in via **xwin** during CI builds.

**Canonical reference for interface definitions:**
https://github.com/microsoft/IIS.Common/blob/main/Include/httpserv_xp.h

When adding new mock classes to `mocks.h`, look up the exact pure virtual method signatures in that header. Follow the existing pattern (e.g. `MockHttpResponse`, `MockHttpContext`) — every pure virtual method must have a `MOCK_METHOD` declaration or a manual override.
