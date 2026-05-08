# Unless explicitly stated otherwise all files in this repository are licensed
# under the Apache 2.0 License. This product includes software developed at
# Datadog (https://www.datadoghq.com/).
#
# Copyright 2024-Present Datadog, Inc.

add_executable(iis_injector_tests)

target_include_directories(iis_injector_tests
  PRIVATE
    ${CMAKE_CURRENT_SOURCE_DIR}/test
)

target_sources(iis_injector_tests
  PRIVATE
    test_entrypoint.cpp
    test_http_module.cpp
    test_injection_pipeline.cpp
)

target_compile_options(iis_injector_tests
  PRIVATE
    /W4
    /DUNICODE
    /bigobj
)

if (IIS_ENABLE_ASAN)
  target_compile_options(iis_injector_tests PRIVATE /fsanitize=address)
  # The object library is not compiled with ASAN (it also builds the DLL which
  # cannot link the ASAN runtime). Disable STL container annotations in the test
  # objects so the annotate_string/annotate_vector metadata matches.
  target_compile_definitions(iis_injector_tests PRIVATE
    _DISABLE_STRING_ANNOTATION=1
    _DISABLE_VECTOR_ANNOTATION=1)
  # clang-cl emits #pragma comment(lib, "clang_rt.asan*-x86_64.lib") refs that
  # native MSVC link.exe resolves automatically; lld-link in our cross-compile
  # setup needs them passed explicitly.
  target_link_libraries(iis_injector_tests
    clang_rt.asan_dynamic-x86_64
    clang_rt.asan_dynamic_runtime_thunk-x86_64)
endif()

# UBSan in trap mode: produces inline traps instead of runtime calls, so it
# works under clang-cl + xwin without a Windows ubsan_runtime library. Caught
# UB aborts with SIGILL / EXCEPTION_ILLEGAL_INSTRUCTION (no diagnostic text).
if (IIS_ENABLE_UBSAN)
  target_compile_options(iis_injector_tests PRIVATE
    -fsanitize=undefined -fsanitize-trap=undefined)
endif()

target_link_libraries(
  iis_injector_tests
  iis_injector::obj
  GTest::gmock
  GTest::gtest
  GTest::gtest_main
  oleaut32)

if (NOT CMAKE_CROSSCOMPILING)
  include(GoogleTest)
  gtest_discover_tests(iis_injector_tests)
else()
  add_test(NAME UnitTest COMMAND iis_injector_tests)
endif ()

