#include "esp_err.h"
#include "esp_log.h"
#include "esp_netif.h"
#include "esp_ota_ops.h"
#include "sdkconfig.h"

#include "audio.h"
#include "config.h"
#include "main.h"
#include "rust.h"
#include "shared.h"
#include "system.h"
#include "was.h"

#ifdef CONFIG_MBEDTLS_SSL_PROTO_TLS1_3
#include "psa/crypto.h"
#endif

#define DEFAULT_WIS_URL "https://infer.tovera.io/api/willow"

#define I2S_PORT I2S_NUM_0

char was_url[2048];
static const char *TAG = "WILLOW/MAIN";

void willow_init(void)
{
    esp_err_t err;

    init_system();
    (void)rust_spiffs_mount();
    config_parse();
    ESP_ERROR_CHECK_WITHOUT_ABORT(rust_display_init());
    ESP_ERROR_CHECK_WITHOUT_ABORT(rust_ui_init());

    ESP_ERROR_CHECK(esp_netif_init());

#ifdef CONFIG_WILLOW_ETHERNET
    rust_ui_show_connecting("Connecting to Ethernet ...");
    ESP_ERROR_CHECK(rust_ethernet_init());
#else
    char psk[64];
    char ssid[33];
    if (!rust_nvs_read_wifi(psk, sizeof(psk), ssid, sizeof(ssid))) {
        goto err_nvs;
    }
    rust_ui_show_connecting("Connecting to Wi-Fi...");

    (void)rust_wifi_init(psk, ssid, &hdl_netif);
#endif

#ifdef CONFIG_MBEDTLS_SSL_PROTO_TLS1_3
    // initialize mbedtls PSA library after wifi to have entropy
    psa_status_t status = psa_crypto_init();
    if (status != PSA_SUCCESS) {
        ESP_LOGE(TAG, "failed to initialize Mbed TLS PSA library, TLS will not work");
    }
#endif

    if (!rust_nvs_read_was_url(was_url, sizeof(was_url))) {
        goto err_nvs;
    }
    err = init_was();
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "failed to initialize Willow Application Server connection");
        rust_ui_show_error("Fatal error!", "WAS initialization failed.");
    }

    if (!config_valid) {
        // wait "indefinitely"
        vTaskDelay(portMAX_DELAY);
    }

// we jump over WAS initialization was without Wi-Fi this will never work
err_nvs:
    if (!rust_state_is_nvs_ok()) {
        rust_ui_show_error("Fatal error!", "Failed to read NVS partition.");
        // wait "indefinitely"
        vTaskDelay(portMAX_DELAY);
    }

    init_audio();
    ESP_ERROR_CHECK_WITHOUT_ABORT(rust_display_timer_init());
    ESP_ERROR_CHECK_WITHOUT_ABORT(rust_ui_touch_init(MSG_STOP));

#ifndef CONFIG_WILLOW_ETHERNET
    rust_get_mac_address(); // should be on wifi by now; print the MAC
#endif

    const esp_app_desc_t *app_desc = esp_app_get_description();
    ESP_LOGI(TAG, "Startup complete! Hardware: %s. Version: %s. Waiting for wake word.", str_hw_type(hw_type),
             app_desc->version);

    // if we reached this point, we can mark the current partition valid
    // we can still crash on wake or other events but we should be able to do another OTA
    // we can also still crash in the while loop below - this should be improved
    ESP_ERROR_CHECK_WITHOUT_ABORT(esp_ota_mark_app_valid_cancel_rollback());

    ESP_ERROR_CHECK_WITHOUT_ABORT(rust_display_timer_reset(false));
}

#ifdef WILLOW_CARGO_FIRST
// esp-idf-sys needs an app_main symbol for its intermediate C link. The final
// Cargo link replaces this weak stub with Rust's strong entry point.
void __attribute__((weak)) app_main(void) {}
#endif
