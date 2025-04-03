# inject-browser-sdk Library

Core library for injecting the Datadog RUM Browser SDK into HTML documents.

## Overview

This library provides the core functionality for parsing HTML documents and injecting the Datadog RUM Browser SDK in the appropriate location. It is used by various web server modules (NGINX, Apache HTTP Server, IIS) to enable automatic RUM injection.

## Features

- Efficient streaming parser that works on chunked HTML responses
- Minimal memory footprint
- Cross-platform compatibility (Linux, Windows, macOS)
- Foreign Function Interface (FFI) for integration with C/C++

## Project Structure

The library is organized into several components:

```
lib/
├── inject-browser-sdk/       # Core injection library
├── inject-browser-sdk-ffi/   # FFI bindings for C/C++
```

Read the full [library reference here](https://datadoghq.dev/inject-browser-sdk).

# Release

1. Create a new tag and release with the prefix `lib-`
