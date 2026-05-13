cmake_minimum_required(VERSION 3.16)
project(logos_shim CXX)

set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_AUTOMOC ON)
set(CMAKE_POSITION_INDEPENDENT_CODE ON)

find_package(Qt6 REQUIRED COMPONENTS Core RemoteObjects)

set(LOGOS_CPP_SDK_DIR "" CACHE PATH "Path to logos-cpp-sdk checkout or installation")
if(NOT LOGOS_CPP_SDK_DIR)
    message(FATAL_ERROR "Set -DLOGOS_CPP_SDK_DIR=<path-to-logos-cpp-sdk>.")
endif()

# Support both old SDK (cpp/CMakeLists.txt source tree) and new SDK (pre-built).
if(EXISTS "${LOGOS_CPP_SDK_DIR}/cpp/CMakeLists.txt")
    # Old SDK layout: build logos_sdk from source via add_subdirectory.
    add_subdirectory(${LOGOS_CPP_SDK_DIR}/cpp ${CMAKE_BINARY_DIR}/_logos_sdk)

    add_library(logos_shim STATIC shim.cpp logos_types_shim.cpp)
    target_include_directories(logos_shim PUBLIC ${CMAKE_CURRENT_SOURCE_DIR})
    target_include_directories(logos_shim PRIVATE
        ${LOGOS_CPP_SDK_DIR}/cpp
        ${LOGOS_CPP_SDK_DIR}/cpp/implementations/qt_remote
        ${LOGOS_CPP_SDK_DIR}/core
    )
    target_link_libraries(logos_shim PUBLIC logos_sdk Qt6::Core Qt6::RemoteObjects)

    install(TARGETS logos_shim ARCHIVE DESTINATION lib)
    install(TARGETS logos_sdk ARCHIVE DESTINATION lib)
else()
    # New SDK layout: pre-built liblogos_sdk.a + flat include/.
    # Use an IMPORTED target to carry the include dirs without triggering
    # find_package dependency resolution (Boost / OpenSSL searches fail outside nix).
    add_library(logos_sdk_prebuilt STATIC IMPORTED GLOBAL)
    set_target_properties(logos_sdk_prebuilt PROPERTIES
        IMPORTED_LOCATION "${LOGOS_CPP_SDK_DIR}/lib/liblogos_sdk.a"
        INTERFACE_INCLUDE_DIRECTORIES
            "${LOGOS_CPP_SDK_DIR}/include;${LOGOS_CPP_SDK_DIR}/include/implementations/qt_local;${LOGOS_CPP_SDK_DIR}/include/implementations/qt_remote;${LOGOS_CPP_SDK_DIR}/include/implementations/mock;${LOGOS_CPP_SDK_DIR}/include/implementations/plain"
    )

    add_library(logos_shim STATIC shim.cpp logos_types_shim.cpp)
    target_include_directories(logos_shim PUBLIC ${CMAKE_CURRENT_SOURCE_DIR})
    target_link_libraries(logos_shim PUBLIC logos_sdk_prebuilt Qt6::Core Qt6::RemoteObjects)

    install(TARGETS logos_shim ARCHIVE DESTINATION lib)
    install(FILES "${LOGOS_CPP_SDK_DIR}/lib/liblogos_sdk.a" DESTINATION lib RENAME liblogos_sdk.a)
endif()
