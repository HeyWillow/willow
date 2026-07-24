idf_component_register(
    PRIV_REQUIRES
        app_update
        esp_audio_codec
        esp_codec_dev
        esp_eth
        esp_http_client
        esp_netif
        esp_websocket_client
        esp_wifi
        fatfs
        nvs_flash
        spiffs
        wear_levelling
)

if(NOT DEFINED ENV{WILLOW_CARGO_FIRST})
    message(FATAL_ERROR "Willow firmware must be built through Cargo")
endif()

spiffs_create_partition_image(user ../../spiffs/user FLASH_IN_PROJECT)

# ESP-SR 1.9.5 packs models in filesystem traversal order. Repack the staged
# models with sorted traversal so clean builds produce an identical image.
idf_build_get_property(WILLOW_BUILD_DIR BUILD_DIR)
idf_component_get_property(ESP_SR_DIR espressif__esp-sr COMPONENT_DIR)
set(WILLOW_SR_PACKER "${CMAKE_CURRENT_LIST_DIR}/../../scripts/pack_srmodels.py")
add_custom_target(
    willow_srmodels_bin ALL
    COMMAND
        "${PYTHON}" "${WILLOW_SR_PACKER}"
        "${ESP_SR_DIR}/model/pack_model.py"
        "${WILLOW_BUILD_DIR}/srmodels"
    DEPENDS "${WILLOW_SR_PACKER}"
    COMMENT "Repack ESP-SR models deterministically"
    VERBATIM
)
add_dependencies(willow_srmodels_bin srmodels_bin)
add_dependencies(flash willow_srmodels_bin)

# sdkconfig sanity checks
if($ENV{WILLOW_SDKCONFIG_SANITY_CHECKS})
    if(NOT CONFIG_WILLOW_ETHERNET)
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
