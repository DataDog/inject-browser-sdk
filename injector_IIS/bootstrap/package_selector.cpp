// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2026-Present Datadog, Inc.

#include "package_selector.h"

#include <windows.h>

#include <cwchar>
#include <utility>

namespace datadog::iis_rum {
namespace {

std::optional<std::wstring> environment(const wchar_t *name) {
  const DWORD required = GetEnvironmentVariableW(name, nullptr, 0);
  if (required == 0 || required > 32767)
    return std::nullopt;
  std::wstring value(required, L'\0');
  const DWORD length = GetEnvironmentVariableW(name, value.data(), required);
  if (length == 0 || length >= required)
    return std::nullopt;
  value.resize(length);
  return value;
}

std::wstring path_join(const std::wstring &base, const wchar_t *suffix) {
  if (base.empty())
    return suffix;
  if (base.back() == L'\\' || base.back() == L'/')
    return base + suffix;
  return base + L'\\' + suffix;
}

bool ordinary_file(const std::wstring &path) {
  const DWORD attributes = GetFileAttributesW(path.c_str());
  return attributes != INVALID_FILE_ATTRIBUTES &&
         (attributes & (FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT)) == 0;
}

std::optional<std::string> read_version(const std::wstring &path) {
  if (!ordinary_file(path))
    return std::nullopt;
  HANDLE file = CreateFileW(
      path.c_str(), GENERIC_READ, FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE, nullptr,
      OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL, nullptr
  );
  if (file == INVALID_HANDLE_VALUE)
    return std::nullopt;
  LARGE_INTEGER size{};
  const bool valid_size = GetFileSizeEx(file, &size) && size.QuadPart > 0 && size.QuadPart <= 256;
  std::string value;
  if (valid_size) {
    value.resize(static_cast<size_t>(size.QuadPart));
    DWORD read = 0;
    if (!ReadFile(file, value.data(), static_cast<DWORD>(value.size()), &read, nullptr) ||
        read != value.size()) {
      value.clear();
    }
  }
  CloseHandle(file);
  if (value.empty() || value.find('\0') != std::string::npos)
    return std::nullopt;
  return value;
}

std::optional<std::wstring> canonical_directory(const std::wstring &path) {
  HANDLE directory = CreateFileW(
      path.c_str(), FILE_READ_ATTRIBUTES, FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
      nullptr, OPEN_EXISTING, FILE_FLAG_BACKUP_SEMANTICS, nullptr
  );
  if (directory == INVALID_HANDLE_VALUE)
    return std::nullopt;
  const DWORD required =
      GetFinalPathNameByHandleW(directory, nullptr, 0, FILE_NAME_NORMALIZED | VOLUME_NAME_DOS);
  std::optional<std::wstring> result;
  if (required > 0 && required < 32767) {
    std::wstring path_buffer(required + 1, L'\0');
    const DWORD length = GetFinalPathNameByHandleW(
        directory, path_buffer.data(), static_cast<DWORD>(path_buffer.size()),
        FILE_NAME_NORMALIZED | VOLUME_NAME_DOS
    );
    if (length > 0 && length < path_buffer.size()) {
      path_buffer.resize(length);
      result = std::move(path_buffer);
    }
  }
  CloseHandle(directory);
  return result;
}

std::optional<std::wstring> package_root() {
  if (auto override_root = environment(L"DD_IIS_RUM_PACKAGE_ROOT"))
    return override_root;
  auto program_data = environment(L"ProgramData");
  if (!program_data)
    return std::nullopt;
  return path_join(*program_data, L"Datadog\\Installer\\packages\\datadog-rum-inject");
}

std::optional<Package> candidate(const std::wstring &root, const wchar_t *slot) {
  const auto directory = path_join(root, slot);
  const auto canonical = canonical_directory(directory);
  if (!canonical)
    return std::nullopt;
  // Open files through the resolved immutable target. Loading through a
  // stable/experiment junction would race a promotion after selection.
  const auto version = read_version(path_join(*canonical, L"version"));
  if (!version)
    return std::nullopt;
  const auto dll = path_join(*canonical, L"iis\\injector_IIS.dll");
  if (!ordinary_file(dll))
    return std::nullopt;
  // Detect a junction retarget while checking the files. If it changed, a
  // subsequent request will select the new target after publication settles.
  const auto canonical_after = canonical_directory(directory);
  if (!canonical_after || _wcsicmp(canonical->c_str(), canonical_after->c_str()) != 0) {
    return std::nullopt;
  }
  return Package{slot, *canonical, dll, *version};
}

}  // namespace

bool Package::same_identity(const Package &other) const {
  return _wcsicmp(canonical_directory.c_str(), other.canonical_directory.c_str()) == 0 &&
         version == other.version;
}

std::optional<Package> select_package() {
  const auto root = package_root();
  if (!root)
    return std::nullopt;
  if (auto experiment = candidate(*root, L"experiment"))
    return experiment;
  return candidate(*root, L"stable");
}

}  // namespace datadog::iis_rum
