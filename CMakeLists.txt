# The following lines of boilerplate have to be in your project's
# CMakeLists in this exact order for cmake to work correctly
cmake_minimum_required(VERSION 3.5)

if(DEFINED ENV{WILLOW_VERSION})
    add_definitions(-DWILLOW_USER_AGENT="Willow/$ENV{WILLOW_VERSION}")
else()
    add_definitions(-DWILLOW_USER_AGENT="Willow/0.1")
endif()

set(EXCLUDE_COMPONENTS
    "adf_utils"
    "app_trace"
    "battery_service"
    "bluetooth_service"
    "cmock"
    "coredump_upload_service"
    "esp_codec_dev"
    "esp_eth"
    "esp_event_cast"
    "esp_gdbstub"
    "esp_hid"
    "esp_http_server"
    "esp_https_ota"
    "esp_https_server"
    "esp_local_ctrl"
    "espcoredump"
    "idf_test"
    "ieee802154"
    "mqtt"
    "openthread"
    "ota_service"
    "perfmon"
    "playlist"
    "protobuf-c"
    "protocomm"
    "touch_element"
    "ulp"
    "unity"
    "usb"
    "wifi_provisioning"
)

include($ENV{ADF_PATH}/CMakeLists.txt)
include($ENV{IDF_PATH}/tools/cmake/project.cmake)

project(willow)
