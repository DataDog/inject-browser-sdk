// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2026-Present Datadog, Inc.

#pragma once

#include <optional>
#include <string>

namespace datadog::iis_rum {

struct Package {
  std::wstring slot;
  std::wstring canonical_directory;
  std::wstring dll_path;
  std::string version;

  bool same_identity(const Package &other) const;
};

// A package is complete only when its slot directory resolves to a real
// directory, version is an ordinary nonempty file, and its IIS DLL is a file.
// Experiment has priority over stable. Called on each request notification so
// deleting the canary takes effect even while Windows pins the loaded image.
std::optional<Package> select_package();

}  // namespace datadog::iis_rum
