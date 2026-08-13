cmake_minimum_required(VERSION 3.20)
project(ani-rife-model-sidecar LANGUAGES C CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

set(RIFE_SOURCE_DIR "" CACHE PATH "Pinned rife-ncnn-vulkan checkout")
if(NOT EXISTS "${RIFE_SOURCE_DIR}/src/rife.cpp")
  message(FATAL_ERROR "RIFE_SOURCE_DIR must point to a complete rife-ncnn-vulkan checkout")
endif()

# The pinned upstream build owns the exact NCNN feature set and shader generation used by RIFE.
add_subdirectory("${RIFE_SOURCE_DIR}/src" "${CMAKE_CURRENT_BINARY_DIR}/rife-upstream")

add_executable(ani-rife-model-sidecar
  src/main.cpp
  "${RIFE_SOURCE_DIR}/src/rife.cpp"
  "${RIFE_SOURCE_DIR}/src/warp.cpp"
)
add_dependencies(ani-rife-model-sidecar generate-spirv)
target_include_directories(ani-rife-model-sidecar PRIVATE
  "${RIFE_SOURCE_DIR}/src"
  "${CMAKE_CURRENT_BINARY_DIR}/rife-upstream"
  "${RIFE_SOURCE_DIR}/src/ncnn/src"
  "${CMAKE_CURRENT_BINARY_DIR}/rife-upstream/ncnn/src"
)
target_link_libraries(ani-rife-model-sidecar PRIVATE ncnn webp Threads::Threads ${Vulkan_LIBRARY})

if(USE_STATIC_MOLTENVK)
  foreach(framework CoreFoundation Foundation Metal QuartzCore CoreGraphics Cocoa IOKit IOSurface)
    find_library(${framework}_FRAMEWORK NAMES ${framework} REQUIRED)
    target_link_libraries(ani-rife-model-sidecar PRIVATE ${${framework}_FRAMEWORK})
  endforeach()
endif()

if(WIN32)
  target_compile_definitions(ani-rife-model-sidecar PRIVATE NOMINMAX WIN32_LEAN_AND_MEAN)
endif()
