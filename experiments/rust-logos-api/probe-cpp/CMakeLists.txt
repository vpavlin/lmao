cmake_minimum_required(VERSION 3.16)
project(agent_info_probe CXX)

# C++20 because logos-cpp-sdk uses std::binary_semaphore + designated
# initialisers in places.
set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_AUTOMOC ON)

# Qt6 — we use QtCore + QtRemoteObjects (LogosAPI is QtRO under the hood).
# Pass `-DCMAKE_PREFIX_PATH=$(nix build .#qt 2>/dev/null || qmake6 -query QT_INSTALL_PREFIX)`
# if these aren't picked up automatically.
find_package(Qt6 REQUIRED COMPONENTS Core RemoteObjects)

# Find logos-cpp-sdk via CMake config or by setting LOGOS_CPP_SDK_DIR.
# The SDK exports `logos_api`, `logos_api_client`, etc. as a single
# consumable target; here we link the source files directly so this
# stays a single-file experiment.
set(LOGOS_CPP_SDK_DIR "" CACHE PATH "Path to logos-cpp-sdk checkout")
if(NOT LOGOS_CPP_SDK_DIR)
    message(FATAL_ERROR
        "Set -DLOGOS_CPP_SDK_DIR=<path-to-logos-cpp-sdk-checkout>. "
        "On the workspace machine that's typically "
        "<workspace>/repos/logos-cpp-sdk.")
endif()

add_executable(agent_info_probe
    main.cpp
    ${LOGOS_CPP_SDK_DIR}/cpp/logos_api.cpp
    ${LOGOS_CPP_SDK_DIR}/cpp/logos_api_client.cpp
    ${LOGOS_CPP_SDK_DIR}/cpp/logos_api_provider.cpp
    ${LOGOS_CPP_SDK_DIR}/cpp/logos_provider_object.cpp
    ${LOGOS_CPP_SDK_DIR}/cpp/logos_registry_factory.cpp
    ${LOGOS_CPP_SDK_DIR}/cpp/implementations/qt_remote/qt_remote_registry.cpp
    ${LOGOS_CPP_SDK_DIR}/cpp/implementations/qt_remote/remote_transport.cpp
)

target_include_directories(agent_info_probe PRIVATE
    ${LOGOS_CPP_SDK_DIR}/cpp
    ${LOGOS_CPP_SDK_DIR}/cpp/implementations/qt_remote
    ${LOGOS_CPP_SDK_DIR}/core
)

target_link_libraries(agent_info_probe PRIVATE
    Qt6::Core
    Qt6::RemoteObjects
)

# If the SDK build flips on rep-file generation, we'd add it here. The
# probe doesn't define any new replicas — it only consumes an existing
# one (`agent`) — so we shouldn't need REPC.
