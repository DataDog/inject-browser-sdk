cmake_minimum_required(VERSION 3.24)
if(CMAKE_SOURCE_DIR STREQUAL CMAKE_CURRENT_SOURCE_DIR)
  project(ddiisrum-bootstrap LANGUAGES CXX)
  option(IIS_BUILD_TESTS "Build bootstrap selector tests" ON)
  enable_testing()
endif()

if(NOT WIN32)
  message(FATAL_ERROR "The IIS RUM bootstrap builds only for Windows")
endif()
if(NOT CMAKE_SIZEOF_VOID_P EQUAL 8)
  message(FATAL_ERROR "The IIS RUM bootstrap currently supports x64 only")
endif()

add_library(ddiisrum-bootstrap SHARED bootstrap.cpp package_selector.cpp)
target_compile_features(ddiisrum-bootstrap PRIVATE cxx_std_17)
target_compile_definitions(ddiisrum-bootstrap PRIVATE WIN32_LEAN_AND_MEAN NOMINMAX)
target_include_directories(ddiisrum-bootstrap PRIVATE ${CMAKE_CURRENT_SOURCE_DIR}/../src)
set_target_properties(ddiisrum-bootstrap PROPERTIES
  OUTPUT_NAME "ddiisrum-bootstrap"
  MSVC_RUNTIME_LIBRARY "MultiThreaded$<$<CONFIG:Debug>:Debug>"
)
if(MSVC)
  target_compile_options(ddiisrum-bootstrap PRIVATE /W4 /WX /sdl /guard:cf)
  target_link_options(ddiisrum-bootstrap PRIVATE /DYNAMICBASE /guard:cf)
endif()

if(IIS_BUILD_TESTS)
  add_executable(ddiisrum-package-selector-test package_selector_test.cpp package_selector.cpp)
  target_compile_features(ddiisrum-package-selector-test PRIVATE cxx_std_17)
  target_compile_definitions(ddiisrum-package-selector-test PRIVATE WIN32_LEAN_AND_MEAN NOMINMAX)
  set_target_properties(ddiisrum-package-selector-test PROPERTIES
    MSVC_RUNTIME_LIBRARY "MultiThreaded$<$<CONFIG:Debug>:Debug>"
  )
  if(MSVC)
    target_compile_options(ddiisrum-package-selector-test PRIVATE /W4 /WX /sdl)
  endif()
  add_test(NAME ddiisrum-package-selector COMMAND ddiisrum-package-selector-test)
endif()
