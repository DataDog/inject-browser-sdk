# Changelog

## [1.4.0](https://github.com/DataDog/inject-browser-sdk/compare/iis-1.3.0...iis-1.4.0) (2026-11-09)


### Features

* **injector:** add configurable early-head injection with an optional closing-head fallback; keep closing-head injection as the default ([#8](https://github.com/DataDog/inject-browser-sdk/issues/8)) ([c55272c](https://github.com/DataDog/inject-browser-sdk/commit/c55272c072a6deefe0a304af3f8b6825effa3582))
* **injector:** add end without padding ([#131](https://github.com/DataDog/inject-browser-sdk/issues/131)) ([cdcc7b8](https://github.com/DataDog/inject-browser-sdk/commit/cdcc7b8f686a8d884e6d092aff491d9d9e403ac8))
* **installer:** add httpd uninstall and harden proxy cleanup ([#109](https://github.com/DataDog/inject-browser-sdk/issues/109)) ([128cf71](https://github.com/DataDog/inject-browser-sdk/commit/128cf712d908cfa8448c2827f2e7f146fdc19425))


### Bug Fixes

* **deps:** vuln github.com/klauspost/compress (patch → v1.20.3) [injector_IIS/test/e2e_tests/go.mod] ([#158](https://github.com/DataDog/inject-browser-sdk/issues/158)) ([39e20ff](https://github.com/DataDog/inject-browser-sdk/commit/39e20ffcc31fe45ce9f756f6e13e7cdc3f9e2093))
* **deps:** vuln minor upgrades — 11 packages (minor: 11) [injector_IIS/test/e2e_tests/go.mod] ([#120](https://github.com/DataDog/inject-browser-sdk/issues/120)) ([63a6812](https://github.com/DataDog/inject-browser-sdk/commit/63a68123e683a00f393f36831dab0c7fd3ff41ea))
* **iis:** inject into responses of any size without copying them ([#153](https://github.com/DataDog/inject-browser-sdk/issues/153)) ([ceba546](https://github.com/DataDog/inject-browser-sdk/commit/ceba54658fe84310a92f9536a064eb3f71ac0820))
* **iis:** keep RUM configuration when upgrading the installer ([#145](https://github.com/DataDog/inject-browser-sdk/issues/145)) ([28fbcf3](https://github.com/DataDog/inject-browser-sdk/commit/28fbcf3d9df82b02abf22622916c1cd0d3d7491f))
* **iis:** derive the MSI ProductVersion BUILD component from CI_PIPELINE_IID so newer pipelines can upgrade installations within the same release ([#93](https://github.com/DataDog/inject-browser-sdk/issues/93)) ([b005f55](https://github.com/DataDog/inject-browser-sdk/commit/b005f5550749fcb501a705b9b85c9aee54f1c57f))
* **iis:** preserve IIS configuration schemas on uninstall so sites with remaining datadogRum configuration continue to work ([#94](https://github.com/DataDog/inject-browser-sdk/issues/94)) ([1370539](https://github.com/DataDog/inject-browser-sdk/commit/1370539924422299761ca5fe7439ce836c261a6b))
* **iis:** serve responses sent in several parts whole ([#150](https://github.com/DataDog/inject-browser-sdk/issues/150)) ([5303f94](https://github.com/DataDog/inject-browser-sdk/commit/5303f94387d34a2baaef9fa71629a2ade83dd666))
* **injector:** honour DD_RUM_ENABLED in stable configuration, report resolved values, and read configuration once when building multiple snippets ([#107](https://github.com/DataDog/inject-browser-sdk/issues/107)) ([2f96574](https://github.com/DataDog/inject-browser-sdk/commit/2f96574d1cc1af815abc026cc38cf56a546efd32))
