cmake_minimum_required(VERSION 3.18)
project(ani-realesrgan-model-sidecar LANGUAGES C CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

if(MSVC)
  # 与上游静态库统一使用静态 MSVC 运行库，避免链接时 RuntimeLibrary 不一致。
  set(CMAKE_MSVC_RUNTIME_LIBRARY "MultiThreaded" CACHE STRING "MSVC runtime linkage")
endif()

find_package(Threads REQUIRED)

set(REALESRGAN_SOURCE_DIR "" CACHE PATH "Pinned Real-ESRGAN-ncnn-vulkan checkout")
if(NOT EXISTS "${REALESRGAN_SOURCE_DIR}/src/realesrgan.cpp")
  message(FATAL_ERROR "REALESRGAN_SOURCE_DIR must point to a complete Real-ESRGAN-ncnn-vulkan checkout")
endif()

# Upstream owns the exact NCNN options and generated Vulkan shaders used by Real-ESRGAN.
add_subdirectory("${REALESRGAN_SOURCE_DIR}/src" "${CMAKE_CURRENT_BINARY_DIR}/realesrgan-upstream")
set_target_properties(realesrgan-ncnn-vulkan PROPERTIES EXCLUDE_FROM_ALL TRUE)

add_executable(ani-realesrgan-model-sidecar
  src/main.cpp
  "${REALESRGAN_SOURCE_DIR}/src/realesrgan.cpp"
)
add_dependencies(ani-realesrgan-model-sidecar generate-spirv)
target_include_directories(ani-realesrgan-model-sidecar PRIVATE
  "${REALESRGAN_SOURCE_DIR}/src"
  "${CMAKE_CURRENT_BINARY_DIR}/realesrgan-upstream"
  "${REALESRGAN_SOURCE_DIR}/src/ncnn/src"
)
target_link_libraries(ani-realesrgan-model-sidecar PRIVATE
  ncnn
  webp
  Threads::Threads
  ${Vulkan_LIBRARY}
)
if(MSVC)
  set_source_files_properties(src/main.cpp PROPERTIES COMPILE_OPTIONS "/EHsc")
else()
  set_source_files_properties(src/main.cpp PROPERTIES COMPILE_OPTIONS "-fexceptions")
endif()
set_target_properties(ani-realesrgan-model-sidecar PROPERTIES CXX_EXTENSIONS OFF)

if(OPENMP_FOUND)
  target_link_libraries(ani-realesrgan-model-sidecar PRIVATE ${OpenMP_CXX_LIBRARIES})
endif()

if(APPLE AND USE_STATIC_MOLTENVK)
  foreach(framework CoreFoundation Foundation Metal QuartzCore CoreGraphics Cocoa IOKit IOSurface)
    find_library(${framework}_FRAMEWORK ${framework})
    target_link_libraries(ani-realesrgan-model-sidecar PRIVATE ${${framework}_FRAMEWORK})
  endforeach()
endif()

if(WIN32)
  target_compile_definitions(ani-realesrgan-model-sidecar PRIVATE NOMINMAX WIN32_LEAN_AND_MEAN)
endif()
