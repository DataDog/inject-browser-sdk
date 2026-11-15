# Unless explicitly stated otherwise all files in this repository are licensed
# under the Apache 2.0 License. This product includes software developed at
# Datadog (https://www.datadoghq.com/).
#
# Copyright 2025-Present Datadog, Inc.

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

