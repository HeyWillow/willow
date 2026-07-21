if(CONFIG_WILLOW_ETHERNET)
    set(WILLOW_ETHERNET_SRC_DIRS "net")
endif()

idf_component_register(
    SRC_DIRS
        "."
        "${WILLOW_ETHERNET_SRC_DIRS}"
    INCLUDE_DIRS
        "."
        "${WILLOW_ETHERNET_SRC_DIRS}"
    PRIV_REQUIRES
        app_update
        audio_board
        audio_hal
        audio_pipeline
        audio_recorder
        audio_sal
        audio_stream
        esp_eth
        esp_http_client
        esp_peripherals
        esp_websocket_client
        input_key_service
        spiffs
)

spiffs_create_partition_image(user ../spiffs/user FLASH_IN_PROJECT)

string(APPEND CMAKE_C_FLAGS -Werror)

# sdkconfig sanity checks
if($ENV{WILLOW_SDKCONFIG_SANITY_CHECKS})
    if (NOT CONFIG_WILLOW_ETHERNET)
        string(REGEX MATCH "^[ -~]+" WIFI_PASSPHRASE "${CONFIG_WIFI_PASSWORD}")
        if(NOT "${CONFIG_WIFI_PASSWORD}" STREQUAL "${WIFI_PASSPHRASE}")
            message(FATAL_ERROR "Wi-Fi WPA passphrase must only contain ASCII characters")
        endif()

        string(LENGTH "${CONFIG_WIFI_PASSWORD}" WIFI_PLEN)
        if(WIFI_PLEN LESS 8 OR WIFI_PLEN GREATER 63)
            message(FATAL_ERROR "Wi-Fi WPA passphrase must be between 8 and 63 ASCII characters")
        endif()
    endif()

endif()
