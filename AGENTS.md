# AGENTS.md

## Project Overview
Datadog RUM Browser SDK auto-injection - injects RUM SDK into HTML responses at proxy/server level.

## Components
- `lib/inject-browser-sdk` - Core Rust library (no_std)
- `lib/inject-browser-sdk-ffi` - FFI bindings
- `cmd/check_conf`, `cmd/test_inject` - CLI tools
- `installer/configurator/` - Go proxy configurator

## Build Commands
```bash
cargo build --release           # Rust
make artifacts                  # Build FFI artifacts (current platform)
make artifacts-all              # Build FFI artifacts (all platforms)
cd installer/configurator && CGO_ENABLED=0 go build
```

## Docker
- `docker-bake.hcl` - Buildx bake config
- `Dockerfile` - Builder image (musl + Windows cross-compile)
- Registry: `ghcr.io/datadog/inject-browser-sdk`
