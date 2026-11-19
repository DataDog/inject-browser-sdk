# Fails when the Rust static library linked into IIS carries the Rust standard
# library. Legacy-mangled std symbols and the panic_abort runtime survive LTO,
# so either marks a std build. Usage: cmake -DLIB=<path> -P check_no_std.cmake
file(STRINGS "${LIB}" std_markers REGEX "_ZN3std|panic_abort" LIMIT_COUNT 1)
if (std_markers)
  message(FATAL_ERROR "${LIB} links the Rust standard library; "
                      "IIS must link the no_std + alloc build (NO_DEFAULT_FEATURES).")
endif ()
message(STATUS "${LIB} is no_std + alloc")
