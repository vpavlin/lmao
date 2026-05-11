cmake_minimum_required(VERSION 3.16)
project(logos_shim CXX)

set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_AUTOMOC ON)
set(CMAKE_POSITION_INDEPENDENT_CODE ON)

find_package(Qt6 REQUIRED COMPONENTS Core RemoteObjects)

set(LOGOS_CPP_SDK_DIR "" CACHE PATH "Path to logos-cpp-sdk checkout")
if(NOT LOGOS_CPP_SDK_DIR)
    message(FATAL_ERROR "Set -DLOGOS_CPP_SDK_DIR=<path-to-logos-cpp-sdk-checkout>.")
endif()

# Consume the SDK's `logos_sdk` STATIC target — same approach as
# probe-cpp/. See ../../README.md for the rationale (cherry-picking
# .cpp files drifts on every SDK update; the SDK already wires Qt +
# Boost + OpenSSL + nlohmann_json correctly).
add_subdirectory(${LOGOS_CPP_SDK_DIR}/cpp ${CMAKE_BINARY_DIR}/_logos_sdk)

add_library(logos_shim STATIC shim.cpp)

target_include_directories(logos_shim PUBLIC
    ${CMAKE_CURRENT_SOURCE_DIR}
)
target_include_directories(logos_shim PRIVATE
    ${LOGOS_CPP_SDK_DIR}/cpp
    ${LOGOS_CPP_SDK_DIR}/cpp/implementations/qt_remote
    ${LOGOS_CPP_SDK_DIR}/core
)

target_link_libraries(logos_shim PUBLIC
    logos_sdk
    Qt6::Core
    Qt6::RemoteObjects
)

# Install both archives where cmake-rs expects them. The SDK target
# normally outputs to `<binary_dir>/lib/liblogos_sdk.a`, but we want
# both side-by-side under the cmake-rs install prefix so build.rs can
# point rustc at a single -L.
install(TARGETS logos_shim ARCHIVE DESTINATION lib)
install(TARGETS logos_sdk ARCHIVE DESTINATION lib)
