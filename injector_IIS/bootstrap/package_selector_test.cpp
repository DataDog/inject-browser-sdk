// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2026-Present Datadog, Inc.

#include "package_selector.h"

#include <windows.h>

#include <filesystem>
#include <fstream>
#include <iostream>
#include <string>

namespace {

namespace fs = std::filesystem;

void write_file(const fs::path &path, const std::string &bytes) {
  std::ofstream file(path, std::ios::binary);
  file.write(bytes.data(), static_cast<std::streamsize>(bytes.size()));
}

bool check(bool condition, const char *name) {
  if (!condition)
    std::cerr << "FAILED: " << name << '\n';
  return condition;
}

}  // namespace

int main() {
  wchar_t temp_path[MAX_PATH]{};
  wchar_t temp_file[MAX_PATH]{};
  if (!GetTempPathW(MAX_PATH, temp_path) || !GetTempFileNameW(temp_path, L"DDR", 0, temp_file)) {
    return 1;
  }
  DeleteFileW(temp_file);
  const fs::path root(temp_file);
  const DWORD previous_length = GetEnvironmentVariableW(L"DD_IIS_RUM_PACKAGE_ROOT", nullptr, 0);
  std::wstring previous(previous_length, L'\0');
  if (previous_length > 0) {
    const DWORD copied =
        GetEnvironmentVariableW(L"DD_IIS_RUM_PACKAGE_ROOT", previous.data(), previous_length);
    previous.resize(copied < previous_length ? copied : 0);
  }
  bool okay = true;
  try {
    SetEnvironmentVariableW(L"DD_IIS_RUM_PACKAGE_ROOT", root.c_str());
    okay &= check(!datadog::iis_rum::select_package(), "absent package");

    const auto stable = root / L"stable";
    fs::create_directories(stable / L"iis");
    write_file(stable / L"iis" / L"injector_IIS.dll", "MZ");
    okay &= check(!datadog::iis_rum::select_package(), "missing version canary");

    write_file(stable / L"version", "");
    okay &= check(!datadog::iis_rum::select_package(), "empty version canary");

    write_file(stable / L"version", std::string("1\0.0", 5));
    okay &= check(!datadog::iis_rum::select_package(), "NUL in version canary");

    write_file(stable / L"version", "1.0.0\n");
    auto selected = datadog::iis_rum::select_package();
    okay &= check(selected && selected->slot == L"stable", "valid stable package");
    if (selected) {
      auto promoted = *selected;
      promoted.slot = L"experiment";
      okay &= check(selected->same_identity(promoted), "same target after slot promotion");
      promoted.version = "2.0.0\n";
      okay &= check(!selected->same_identity(promoted), "changed version identity");
      promoted = *selected;
      promoted.canonical_directory += L"-different";
      okay &= check(!selected->same_identity(promoted), "changed target identity");
    }

    const auto experiment = root / L"experiment";
    fs::create_directories(experiment / L"iis");
    write_file(experiment / L"version", "2.0.0\n");
    write_file(experiment / L"iis" / L"injector_IIS.dll", "MZ");
    selected = datadog::iis_rum::select_package();
    okay &= check(selected && selected->slot == L"experiment", "experiment priority");

    write_file(experiment / L"version", std::string(257, 'x'));
    selected = datadog::iis_rum::select_package();
    okay &= check(selected && selected->slot == L"stable", "malformed experiment fallback");
  } catch (const std::exception &error) {
    std::cerr << error.what() << '\n';
    okay = false;
  }
  if (previous_length > 0) {
    SetEnvironmentVariableW(L"DD_IIS_RUM_PACKAGE_ROOT", previous.c_str());
  } else {
    SetEnvironmentVariableW(L"DD_IIS_RUM_PACKAGE_ROOT", nullptr);
  }
  std::error_code cleanup_error;
  fs::remove_all(root, cleanup_error);
  return okay && !cleanup_error ? 0 : 1;
}
