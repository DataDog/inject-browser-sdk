# IIS: accuracy and throughput of the existing native module

`run-iis.ps1` runs the same page set and the same checks as `bench/run.sh` does for nginx and
httpd, against the native module in `injector_IIS/` (installed from its MSI). **It has not been
run**: it was written without a Windows host, so expect a shakedown on the first run.

## Prerequisites (on the Windows host)

- IIS with the static content feature, plus **URL Rewrite** and **Application Request Routing
  (ARR)** (for the proxied `cl`/`chunked` modes).
- The IIS module installed: `injector_IIS.msi` (registers the global module
  `DDIISInjectorModule` and the `datadogRum` config schema). Pass `-ModuleName` if yours differs.
- Python 3 on `PATH` (`-Python` otherwise): the page generator, the random-chunk upstream and the
  raw-socket accuracy checker are the same scripts as on Linux.
- The oracle: either Rust + MSVC on the host (the script runs `cargo run -p bench-tools --bin
  oracle` in `bench/`), or a prebuilt `oracle.exe` passed with `-Oracle`.
- [`oha`](https://github.com/hatoo/oha/releases) for the load (`-Oha C:\path\oha.exe`).

## Run

From an elevated PowerShell at the repository root:

```powershell
.\bench\iis\run-iis.ps1 -Oha C:\tools\oha.exe                 # accuracy, then perf
.\bench\iis\run-iis.ps1 -Mode accuracy -Seeds 8
.\bench\iis\run-iis.ps1 -Mode perf -Conns 16,64,256 -Duration 10s
```

It creates (and replaces) the site `rum-bench` on `127.0.0.1:8080` with its own app pool, rooted at
`C:\rum-bench\pages`, and sets ARR's global `responseBufferLimit` to 0 so the upstream writes
stream through. Results go to `bench\results\`: `accuracy-iis-on.json`, `perf-iis.jsonl`,
`perf-iis.md`.

## What it checks

- **Accuracy.** A calibration page (`<html><head></head>...`) gives the exact snippet IIS builds
  from the site's `datadogRum` options; the oracle then runs the library's `Injector` over each
  whole page with that snippet. Every page is fetched raw (`Connection: close`) as a static file
  and through ARR from `upstream.py` in random-size writes, with `Content-Length` (`cl`) and
  chunked (`chunked`), 4 seeds each by default. The body must equal the oracle output, the
  `Content-Length` must equal the bytes on the wire, and nothing may follow the body. IIS never
  pads (when it does not inject, it serves the body as is), so every mode expects the unpadded
  output. A mismatch on a streamed (`cl`/`chunked`) page whose `<head>` is resolved only after
  the headers went out may be the module's documented behavior rather than a bug: check the
  failure details.
- **Perf.** `off` removes the module from the site (`<modules><remove>`); `on` enables
  `datadogRum`. For each perf page and connection count: req/s, p50/p99 (oha) and the `w3wp` CPU
  time per request (kernel time in `http.sys` is not counted). Run the load generator on other
  cores than IIS where possible, and compare relative numbers only.
