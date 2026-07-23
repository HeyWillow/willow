#include "esp_err.h"
#include "esp_log.h"
#include "esp_netif.h"
#include "esp_ota_ops.h"
#include "esp_timer.h"
#include "nvs_flash.h"
#include "sdkconfig.h"

#include "audio.h"
#include "config.h"
#include "main.h"
#include "network.h"
#include "rust.h"
#include "shared.h"
#include "tasks.h"
#include "was.h"

#ifdef CONFIG_MBEDTLS_SSL_PROTO_TLS1_3
#include "psa/crypto.h"
#endif

#if defined(CONFIG_WILLOW_ETHERNET)
#include "net/ethernet.h"
#endif

#define DEFAULT_WIS_URL "https://infer.tovera.io/api/willow"

#define I2S_PORT I2S_NUM_0

char was_url[2048];
static const char *TAG = "WILLOW/MAIN";

void willow_init(void)
{
    esp_err_t err;

    config_parse();
    ESP_ERROR_CHECK_WITHOUT_ABORT(rust_display_init());
    ESP_ERROR_CHECK_WITHOUT_ABORT(rust_ui_init());

    ESP_ERROR_CHECK(esp_netif_init());

    err = nvs_flash_init();
    if (err == ESP_ERR_NVS_NO_FREE_PAGES) {
        // NVS partition was truncated and needs to be erased
        // Retry nvs_flash_init
        ESP_ERROR_CHECK(nvs_flash_erase());
        ESP_ERROR_CHECK(nvs_flash_init());
    }

    nvs_handle_t hdl_nvs;
    size_t sz;

#ifdef CONFIG_WILLOW_ETHERNET
    init_ethernet();
#else
    err = nvs_open("WIFI", NVS_READONLY, &hdl_nvs);
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "failed to open NVS namespace WIFI: %s", esp_err_to_name(err));
        goto err_nvs;
    }

    char psk[64];
    sz = sizeof(psk);
    err = nvs_get_str(hdl_nvs, "PSK", psk, &sz);
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "failed to get PSK from NVS namespace WIFI: %s", esp_err_to_name(err));
        goto err_nvs;
    }

    char ssid[33];
    sz = sizeof(ssid);
    err = nvs_get_str(hdl_nvs, "SSID", ssid, &sz);
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "failed to get PSK from NVS namespace WIFI: %s", esp_err_to_name(err));
        goto err_nvs;
    }
    init_wifi(psk, ssid);
#endif

    err = nvs_open("WAS", NVS_READONLY, &hdl_nvs);
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "failed to open NVS namespace WAS: %s", esp_err_to_name(err));
        goto err_nvs;
    }

#ifdef CONFIG_MBEDTLS_SSL_PROTO_TLS1_3
    // initialize mbedtls PSA library after wifi to have entropy
    psa_status_t status = psa_crypto_init();
    if (status != PSA_SUCCESS) {
        ESP_LOGE(TAG, "failed to initialize Mbed TLS PSA library, TLS will not work");
    }
#endif

    sz = sizeof(was_url);
    err = nvs_get_str(hdl_nvs, "URL", was_url, &sz);
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "failed to get WASL URL from NVS namespace WAS: %s", esp_err_to_name(err));
        goto err_nvs;
    }
    rust_state_mark_nvs_ok();
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
    get_mac_address(); // should be on wifi by now; print the MAC
#endif

    const esp_app_desc_t *app_desc = esp_app_get_description();
    ESP_LOGI(TAG, "Startup complete! Hardware: %s. Version: %s. Waiting for wake word.",
             rust_system_hardware_name(),
             app_desc->version);

    // if we reached this point, we can mark the current partition valid
    // we can still crash on wake or other events but we should be able to do another OTA
    // we can also still crash in the while loop below - this should be improved
    ESP_ERROR_CHECK_WITHOUT_ABORT(esp_ota_mark_app_valid_cancel_rollback());

    ESP_ERROR_CHECK_WITHOUT_ABORT(rust_display_timer_reset(false));

#ifdef CONFIG_WILLOW_DEBUG_RUNTIME_STATS
    xTaskCreate(&task_debug_runtime_stats, "dbg_runtime_stats", 4 * 1024, NULL, 0, NULL);
#endif
}

void willow_main_loop_iteration(void)
{
#ifdef CONFIG_WILLOW_DEBUG_MEM
    printf("MALLOC_CAP_INTERNAL:\n");
    heap_caps_print_heap_info(MALLOC_CAP_INTERNAL);
    printf("MALLOC_CAP_SPIRAM:\n");
    heap_caps_print_heap_info(MALLOC_CAP_SPIRAM);
#endif
#ifdef CONFIG_WILLOW_DEBUG_TASKS
    char buf[128];
    vTaskList(buf);
    printf("%s\n", buf);
#endif
#ifdef CONFIG_WILLOW_DEBUG_TIMERS
    (esp_timer_dump(stdout));
#endif
    vTaskDelay(5000 / portTICK_PERIOD_MS);
}

#ifdef WILLOW_CARGO_FIRST
// esp-idf-sys needs an app_main symbol for its intermediate C link. The final
// Cargo link replaces this weak stub with Rust's strong entry point.
void __attribute__((weak)) app_main(void) {}
#endif
