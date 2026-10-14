cmake_minimum_required(VERSION 3.21)
project(heif-png CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_MSVC_RUNTIME_LIBRARY "MultiThreaded")
set(DECODER "${CMAKE_CURRENT_SOURCE_DIR}/../../extractous-core/tika-native/src/static")

find_package(libheif CONFIG REQUIRED)
find_package(leptonica CONFIG REQUIRED)

add_executable(heif-png main.cc "${DECODER}/heif.cc")
target_include_directories(heif-png PRIVATE "${DECODER}")
target_link_libraries(heif-png PRIVATE heif leptonica)
