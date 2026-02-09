cmake_minimum_required(VERSION 3.22)
project(consumer_test LANGUAGES C CXX)

include(FetchContent)

FetchContent_Declare(
  InjectBrowserSDK
  SOURCE_DIR ${CMAKE_CURRENT_SOURCE_DIR}/sdk
)
FetchContent_MakeAvailable(InjectBrowserSDK)

add_executable(consumer_test main.c)
target_link_libraries(consumer_test PRIVATE inject_browser_sdk_ffi)

option(ENABLE_ASAN "Build with AddressSanitizer" OFF)
if(ENABLE_ASAN)
  target_compile_options(consumer_test PRIVATE -fsanitize=address -fno-omit-frame-pointer)
  target_link_options(consumer_test PRIVATE -fsanitize=address)
endif()
