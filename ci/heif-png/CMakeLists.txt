cmake_minimum_required(VERSION 3.21)
project(heif-png CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_MSVC_RUNTIME_LIBRARY "MultiThreaded")
set(DECODER "${CMAKE_CURRENT_SOURCE_DIR}/../../extractous-core/tika-native/src/static")

find_package(libheif CONFIG REQUIRED)
find_package(Leptonica CONFIG REQUIRED)
find_path(LEPTONICA_HEADERS allheaders.h PATH_SUFFIXES leptonica REQUIRED)

add_executable(heif-png main.cc "${DECODER}/heif.cc")
target_include_directories(heif-png PRIVATE "${DECODER}" "${LEPTONICA_HEADERS}")
target_link_libraries(heif-png PRIVATE heif leptonica)
if(UNIX AND NOT APPLE)
  target_link_options(heif-png PRIVATE -static)
endif()
set_target_properties(heif-png PROPERTIES RUNTIME_OUTPUT_DIRECTORY "$<1:${CMAKE_BINARY_DIR}/out>")
