cmake_minimum_required(VERSION 3.21)
project(extractous-tesseract CXX)

if(NOT TESSERACT_SOURCE)
  message(FATAL_ERROR "TESSERACT_SOURCE must name tesseract's source tree")
endif()
set(JAVA_HOME "$ENV{JAVA_HOME}")
if(NOT EXISTS "${JAVA_HOME}/include/jni.h")
  message(FATAL_ERROR "JAVA_HOME (${JAVA_HOME}) must name a JDK with JNI headers")
endif()
if(WIN32)
  set(JNI_PLATFORM win32)
elseif(APPLE)
  set(JNI_PLATFORM darwin)
else()
  set(JNI_PLATFORM linux)
endif()

set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_MSVC_RUNTIME_LIBRARY "MultiThreaded")
set(CMAKE_POSITION_INDEPENDENT_CODE ON)
set(BUILD_SHARED_LIBS OFF)
foreach(option BUILD_TRAINING_TOOLS INSTALL_CONFIGS ENABLE_CCACHE)
  set(${option} OFF CACHE BOOL "" FORCE)
endforeach()
foreach(option DISABLE_ARCHIVE DISABLE_CURL GRAPHICS_DISABLED)
  set(${option} ON CACHE BOOL "" FORCE)
endforeach()
add_subdirectory("${TESSERACT_SOURCE}" tesseract EXCLUDE_FROM_ALL)

set(SHIM "${CMAKE_CURRENT_SOURCE_DIR}/../../extractous-core/tika-native/src/static")
find_path(LEPTONICA_HEADERS allheaders.h PATH_SUFFIXES leptonica REQUIRED)
add_library(extractous_tesseract SHARED "${SHIM}/tesseract.cc")
target_include_directories(extractous_tesseract PRIVATE
  "${JAVA_HOME}/include" "${JAVA_HOME}/include/${JNI_PLATFORM}"
  "${CMAKE_CURRENT_BINARY_DIR}/tesseract" "${CMAKE_CURRENT_BINARY_DIR}/tesseract/include"
  "${LEPTONICA_HEADERS}")
get_directory_property(TESSERACT_DEFINITIONS DIRECTORY "${TESSERACT_SOURCE}" COMPILE_DEFINITIONS)
target_compile_definitions(extractous_tesseract PRIVATE ${TESSERACT_DEFINITIONS})
target_link_libraries(extractous_tesseract PRIVATE libtesseract)
if(UNIX AND NOT APPLE)
  target_link_options(extractous_tesseract PRIVATE -static-libstdc++ -static-libgcc -Wl,-Bsymbolic
    -Wl,--exclude-libs,ALL)
endif()
set_target_properties(extractous_tesseract PROPERTIES
  CXX_VISIBILITY_PRESET hidden
  VISIBILITY_INLINES_HIDDEN ON
  LIBRARY_OUTPUT_DIRECTORY "$<1:${CMAKE_BINARY_DIR}/out>"
  RUNTIME_OUTPUT_DIRECTORY "$<1:${CMAKE_BINARY_DIR}/out>")
