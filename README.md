# Datadog RUM Browser SDK Auto-Injection

A collection of libraries and modules to automatically inject the [Datadog RUM Browser SDK](https://docs.datadoghq.com/real_user_monitoring/browser/) into HTML pages served by various web servers and proxies.

## Overview

This project enables automatic injection of the Datadog RUM Browser SDK into HTML responses without requiring changes to your application code. It works by intercepting HTML responses and inserting the SDK snippet before the closing `</head>` tag.

Supported web servers and proxies:

- **NGINX**
- **Apache HTTP Server**
- **Microsoft IIS**

## Key Features

- **Zero Code Changes**: No need to modify your application code
- **Performance Optimized**: Minimal impact on response times
- **Flexible Configuration**: Configure per site or application
- **Telemetry**: Basic telemetry to help diagnose issues

## Architecture

The project consists of several components:

1. **Core Injection Library**: Written in Rust, handling the parsing and injection of the SDK
2. **Web Server Module**: Native module for IIS (Other proxies/web servers may live in different repos)
3. **Installer Scripts**: For easy deployment and configuration

All modules use a shared core library (`inject-browser-sdk`) to ensure consistent behavior across web servers.

## Development

For detailed build instructions for specific modules, see their respective README files:

- [NGINX/httpd installer](installer/README.md)
- [IIS Module](injector_IIS/README.md)

## Contributing

We welcome contributions to the project! Please see our [Contributing Guide](CONTRIBUTING.md) for more information.

## License

This project is licensed under the Apache 2.0 License - see the [LICENSE](LICENSE) file for details.

## Support

For support, please [open an issue](https://github.com/DataDog/inject-browser-sdk/issues/new) or contact [Datadog support](https://docs.datadoghq.com/help/).
