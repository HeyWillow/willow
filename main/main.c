#include "esp_err.h"
#include "esp_log.h"
#include "sdkconfig.h"

#include "audio.h"
#include "config.h"
#include "main.h"
#include "rust.h"
#include "shared.h"
#include "was.h"

#define DEFAULT_WIS_URL "https://infer.tovera.io/api/willow"

#define I2S_PORT I2S_NUM_0

char was_url[2048];
static const char *TAG = "WILLOW/MAIN";

void willow_init(void)
{
    esp_err_t err;

    ESP_ERROR_CHECK(rust_network_init(&hdl_netif));

    if (!rust_nvs_read_was_url(was_url, sizeof(was_url))) {
        rust_ui_show_error("Fatal error!", "Failed to read NVS partition.");
        for (;;) {
            vTaskDelay(portMAX_DELAY);
        }
    }
    err = init_was();
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "failed to initialize Willow Application Server connection");
        rust_ui_show_error("Fatal error!", "WAS initialization failed.");
    }

    if (!rust_config_is_valid()) {
        // wait "indefinitely"
        vTaskDelay(portMAX_DELAY);
    }

    init_audio();
    ESP_ERROR_CHECK_WITHOUT_ABORT(rust_display_timer_init());
    ESP_ERROR_CHECK_WITHOUT_ABORT(rust_ui_touch_init(MSG_STOP));

#ifndef CONFIG_WILLOW_ETHERNET
    rust_get_mac_address(); // should be on wifi by now; print the MAC
#endif
}

#ifdef WILLOW_CARGO_FIRST
// esp-idf-sys needs an app_main symbol for its intermediate C link. The final
// Cargo link replaces this weak stub with Rust's strong entry point.
void __attribute__((weak)) app_main(void) {}
#endif
