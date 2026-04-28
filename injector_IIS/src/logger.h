/*
 * Unless explicitly stated otherwise all files in this repository are licensed
 * under the Apache 2.0 License. This product includes software developed at
 * Datadog (https://www.datadoghq.com/).
 *
 * Copyright 2024-Present Datadog, Inc.
 */

#pragma once
#include "event_provider.h"
#include "framework.h"
#include <algorithm>
#include <datadog/error.h>
#include <datadog/logger.h>
#include <format>
#include <sstream>
#include <string>

namespace datadog::rum {

enum class LogLevel { Error = 0, Info = 1, Debug = 2 };

// Logger using the Event Logging API
// <https://learn.microsoft.com/en-us/windows/win32/eventlog/event-logging>
//
// TODO(@dmehala): Refactor to use the Windows Event Log API
// <https://learn.microsoft.com/en-us/windows/win32/wes/windows-event-log>
class Logger : public datadog::tracing::Logger {
  HANDLE m_eventLog;
  std::ostringstream m_stream;
  LogLevel m_logLevel;

 public:
  Logger(LogLevel initial_level = LogLevel::Info) : m_logLevel(initial_level) {
    m_eventLog = RegisterEventSourceW(nullptr, L"Datadog-RUM-Instrumentation");
    if (m_eventLog == nullptr) {
      OutputDebugStringW(
          L"Datadog RUM: Failed to register event source — logging disabled");
    }
  }

  ~Logger() override {
    if (m_eventLog != nullptr) {
      DeregisterEventSource(m_eventLog);
      m_eventLog = nullptr;
    }
  }

  void set_log_level(LogLevel level) { m_logLevel = level; }

  inline bool should_log(LogLevel level) const {
    return static_cast<int>(level) <= static_cast<int>(m_logLevel);
  }

  virtual void info(const std::string& message) {
    if (!should_log(LogLevel::Info)) return;

    report_event(message, EVENTLOG_INFORMATION_TYPE, INJECTOR_CATEGORY,
                 MSG_GENERIC_INFO);
  }

  virtual void debug(const std::string& message) {
    if (!should_log(LogLevel::Debug)) return;

    report_event(message, EVENTLOG_INFORMATION_TYPE, INJECTOR_CATEGORY,
                 MSG_GENERIC_DEBUG);
  }

  virtual void error(const std::string& message) {
    if (!should_log(LogLevel::Error)) return;

    report_event(message, EVENTLOG_ERROR_TYPE, INJECTOR_CATEGORY,
                 MSG_GENERIC_ERROR);
  }

  void report_event(const std::string& message, WORD type, WORD category,
                    DWORD id) const {
    if (m_eventLog == nullptr) return;

    const int wide_len =
        MultiByteToWideChar(CP_UTF8, 0, message.c_str(),
                            static_cast<int>(message.size()), nullptr, 0);
    if (wide_len <= 0) {
      LPCWSTR fallback = L"[UTF-8 conversion failed]";
      ReportEventW(m_eventLog, type, category, id, nullptr, 1, 0, &fallback,
                   nullptr);
      return;
    }

    std::wstring wide(wide_len, L'\0');
    if (MultiByteToWideChar(CP_UTF8, 0, message.c_str(),
                            static_cast<int>(message.size()), wide.data(),
                            wide_len) == 0) {
      LPCWSTR fallback = L"[UTF-8 conversion failed]";
      ReportEventW(m_eventLog, type, category, id, nullptr, 1, 0, &fallback,
                   nullptr);
      return;
    }

    LPCWSTR szNotification = wide.c_str();
    ReportEventW(m_eventLog, type, category, id, nullptr, 1, 0, &szNotification,
                 nullptr);
  }

  // Note(@dmehala): Don't print the tracer startup configuration.
  void log_startup(const LogFunc&) override {}

  void log_error(const LogFunc& f) override {
    if (!should_log(LogLevel::Error)) return;

    m_stream.clear();
    m_stream.str("");

    f(m_stream);
    report_event(m_stream.str(), EVENTLOG_ERROR_TYPE, TRACER_CATEGORY,
                 MSG_GENERIC_ERROR);
  }

  void log_error(const datadog::tracing::Error& error) override {
    if (!should_log(LogLevel::Error)) return;

    report_event(std::format("[dd-trace-cpp error code {}] {}",
                             static_cast<int>(error.code), error.message),
                 EVENTLOG_ERROR_TYPE, TRACER_CATEGORY, MSG_GENERIC_ERROR);
  }

  void log_error(datadog::tracing::StringView sv) override {
    if (!should_log(LogLevel::Error)) return;

    report_event(std::string(sv), EVENTLOG_ERROR_TYPE, TRACER_CATEGORY,
                 MSG_GENERIC_ERROR);
  }
};
}  // namespace datadog::rum
