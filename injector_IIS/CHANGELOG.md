# Changelog

## [1.5.0](https://github.com/DataDog/inject-browser-sdk/compare/iis-1.4.0...iis-1.5.0) (2026-11-16)


### Features

* **iis:** carry the injector over the parts of a response ([#163](https://github.com/DataDog/inject-browser-sdk/issues/163)) ([e30c691](https://github.com/DataDog/inject-browser-sdk/commit/e30c691944d912f7f1b518d49de49080ab2f368b))
* **iis:** inject into static files ([#169](https://github.com/DataDog/inject-browser-sdk/issues/169)) ([b60702a](https://github.com/DataDog/inject-browser-sdk/commit/b60702ab41c3719e96513e5ca38ff5bfbea76fbe))
* **injector:** inject in &lt;head&gt; only, before &lt;/head&gt; by default or as early as possible ([#159](https://github.com/DataDog/inject-browser-sdk/issues/159)) ([dfc25c5](https://github.com/DataDog/inject-browser-sdk/commit/dfc25c59ace22ef51b3369de8b5d06771cc996bc))


### Bug Fixes

* **config:** read DD_RUM_TRACK_USER_INTERACTION ([#166](https://github.com/DataDog/inject-browser-sdk/issues/166)) ([72384af](https://github.com/DataDog/inject-browser-sdk/commit/72384af4c90b2350e9182d3b5c53f125b4496a4e))
* **injector:** end a character reference at the next tag in the head walk ([#179](https://github.com/DataDog/inject-browser-sdk/issues/179)) ([cb5b199](https://github.com/DataDog/inject-browser-sdk/commit/cb5b19935ed75f44c9f0564af8b590b0773e31b0))
* **injector:** same output whatever the chunking (fuzz findings) ([#177](https://github.com/DataDog/inject-browser-sdk/issues/177)) ([6e7e6e7](https://github.com/DataDog/inject-browser-sdk/commit/6e7e6e75a9f3d100a38ac3a02d311bc628903a33))
* **snippet:** escape HTML-sensitive chars in the init JSON ([#175](https://github.com/DataDog/inject-browser-sdk/issues/175)) ([f710d8e](https://github.com/DataDog/inject-browser-sdk/commit/f710d8eddcaf52028f5ad5a4d1dcdbc3736bd426))

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
