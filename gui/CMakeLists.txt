# flattencom - Qt Build and Packaging Configuration
#
# Builds the Qt client and tests, installs resources and configures CPack artifacts.
#
# Authors:
# worryzu <worryzu@gmail.com> @LinearTeam
#
# Copyright (C) 2026 Evarentha
# SPDX-License-Identifier: GPL-3.0-or-later

cmake_minimum_required(VERSION 3.24)
# Cargo.toml is the single source of the project version. Every Qt, install and
# CPack version is derived from it so a release cannot advertise a stale number.
file(READ "${CMAKE_CURRENT_SOURCE_DIR}/../Cargo.toml" FLATTENCOM_CARGO_MANIFEST)
string(REGEX MATCHALL "\nversion = \"([^\"]+)\"" FLATTENCOM_DECLARED_VERSIONS "${FLATTENCOM_CARGO_MANIFEST}")
list(LENGTH FLATTENCOM_DECLARED_VERSIONS FLATTENCOM_DECLARED_VERSION_COUNT)
if(NOT FLATTENCOM_DECLARED_VERSION_COUNT EQUAL 1)
    message(FATAL_ERROR "Cargo.toml must declare exactly one line-initial version, found ${FLATTENCOM_DECLARED_VERSION_COUNT}")
endif()
list(GET FLATTENCOM_DECLARED_VERSIONS 0 FLATTENCOM_DECLARED_VERSION)
string(REGEX REPLACE "\nversion = \"([^\"]+)\"" "\\1" FLATTENCOM_VERSION "${FLATTENCOM_DECLARED_VERSION}")
project(flattencom-gui VERSION ${FLATTENCOM_VERSION} LANGUAGES CXX)
# Published for verification against Cargo.toml once this tree is configured.
set(FLATTENCOM_VERSION "${FLATTENCOM_VERSION}" CACHE INTERNAL "Project version read from the Cargo workspace manifest")
include(GNUInstallDirs)
set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_AUTOMOC ON)
set(CMAKE_AUTORCC ON)
find_package(Qt6 6.5 REQUIRED COMPONENTS Widgets Network)
add_library(flattencom-ui STATIC
    src/AboutDialog.cpp src/AboutDialog.h
    src/RpcClient.cpp src/RpcClient.h
    src/FramesModel.cpp src/FramesModel.h
    src/TextStreamView.cpp src/TextStreamView.h
    src/SessionPane.cpp src/SessionPane.h
    src/SessionController.cpp src/SessionController.h
    src/SessionAnalysis.cpp
    src/IssueReport.cpp src/IssueReport.h
    src/SentLog.cpp src/SentLog.h
    src/MainWindow.cpp src/MainWindow.h
    src/InspectorDetails.cpp src/InspectorDetails.h
    src/Style.cpp src/Style.h
    src/Icons.cpp src/Icons.h
    src/Recording.cpp src/Recording.h
    src/LogRules.cpp src/LogRules.h
    src/RuleEditors.cpp src/RuleEditors.h
    src/DeviceProfiles.cpp src/DeviceProfiles.h
    src/LogManager.cpp src/LogManager.h
    src/CaptureViewer.cpp src/CaptureViewer.h
    src/SessionHistory.cpp src/SessionHistory.h
    resources/resources.qrc
    src/RateChart.cpp src/RateChart.h)
target_link_libraries(flattencom-ui PUBLIC Qt6::Widgets Qt6::Network)
target_include_directories(flattencom-ui PUBLIC src)
target_compile_definitions(flattencom-ui PUBLIC FLATTENCOM_VERSION="${FLATTENCOM_VERSION}")
if(MSVC)
    target_compile_options(flattencom-ui PRIVATE /W4 /utf-8)
else()
    target_compile_options(flattencom-ui PRIVATE -Wall -Wextra -Wpedantic)
endif()
add_executable(flattencom-gui WIN32 src/main.cpp)
if(WIN32)
    enable_language(RC)
    target_sources(flattencom-gui PRIVATE resources/application.rc)
    target_include_directories(flattencom-gui PRIVATE resources)
    set_source_files_properties(resources/application.rc PROPERTIES
        OBJECT_DEPENDS "${CMAKE_CURRENT_SOURCE_DIR}/resources/branding/flattencom.ico")
endif()
target_link_libraries(flattencom-gui PRIVATE flattencom-ui)
if(MSVC)
    target_compile_options(flattencom-gui PRIVATE /utf-8)
endif()
include(CTest)
if(BUILD_TESTING)
    find_package(Qt6 REQUIRED COMPONENTS Test)
    add_executable(gui-tests tests/models.cpp)
    target_compile_definitions(gui-tests PRIVATE FIXTURE_PATH="${CMAKE_CURRENT_SOURCE_DIR}/../crates/flattencom-proto/fixtures/frame-page.json")
    if(MSVC)
        target_compile_options(gui-tests PRIVATE /utf-8)
    endif()
    target_link_libraries(gui-tests PRIVATE flattencom-ui Qt6::Test)
    add_test(NAME gui-models COMMAND gui-tests -platform offscreen)
endif()
install(TARGETS flattencom-gui RUNTIME DESTINATION bin)
set(FLATTENCOM_RUST_BIN_DIR "${CMAKE_CURRENT_SOURCE_DIR}/../target/release" CACHE PATH "Directory containing the release Rust binaries")
option(FLATTENCOM_BUNDLE_RUST "Include daemon, CLI and MCP binaries in install/package" OFF)
option(FLATTENCOM_DEPLOY_QT "Deploy Qt runtime dependencies" OFF)
if(FLATTENCOM_BUNDLE_RUST)
    foreach(name flattencom flattencomd flattencom-mcp)
        install(PROGRAMS "${FLATTENCOM_RUST_BIN_DIR}/${name}${CMAKE_EXECUTABLE_SUFFIX}" DESTINATION bin)
    endforeach()
endif()
if(FLATTENCOM_DEPLOY_QT)
    qt_generate_deploy_app_script(TARGET flattencom-gui OUTPUT_SCRIPT deploy_script NO_UNSUPPORTED_PLATFORM_ERROR)
    install(SCRIPT ${deploy_script})
endif()
install(FILES "${CMAKE_CURRENT_SOURCE_DIR}/../LICENSE" DESTINATION share/licenses/flattencom)
install(DIRECTORY "${CMAKE_CURRENT_SOURCE_DIR}/../docs/" DESTINATION share/doc/flattencom)
if(UNIX)
    install(FILES "${CMAKE_CURRENT_SOURCE_DIR}/../packaging/linux/flattencom.desktop" DESTINATION share/applications)
    install(FILES "${CMAKE_CURRENT_SOURCE_DIR}/../packaging/linux/flattencom.svg" DESTINATION share/icons/hicolor/scalable/apps)
endif()
set(CPACK_PACKAGE_NAME flattencom)
set(CPACK_PACKAGE_VENDOR "Evarentha")
set(CPACK_PACKAGE_VERSION ${PROJECT_VERSION})
set(CPACK_RESOURCE_FILE_LICENSE "${CMAKE_CURRENT_SOURCE_DIR}/../LICENSE")
set(CPACK_PACKAGE_CONTACT "Evarentha")
set(CPACK_PACKAGE_DESCRIPTION_SUMMARY "Serial debugging workbench with Qt, CLI/TUI and MCP interfaces")
set(CPACK_PACKAGE_INSTALL_DIRECTORY "flattencom")
set(CPACK_PACKAGE_EXECUTABLES "flattencom-gui;flattencom")
set(CPACK_DEBIAN_PACKAGE_MAINTAINER "Evarentha")
set(CPACK_DEBIAN_PACKAGE_SHLIBDEPS ON)
if(WIN32)
    set(CPACK_GENERATOR "ZIP;WIX")
    set(CPACK_WIX_UPGRADE_GUID "2D49EBDE-53F4-4B82-820D-E68A40A4412D")
else()
    set(CPACK_GENERATOR "TGZ")
endif()
include(CPack)
