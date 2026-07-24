#include "esp_err.h"
#include "esp_log.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"

#include "main.h"
#include "rust.h"

char was_url[2048];
static const char *TAG = "WILLOW/MAIN";

void willow_init(void)
{
    esp_err_t err;

    ESP_ERROR_CHECK(rust_network_init());

    if (!rust_nvs_read_was_url(was_url, sizeof(was_url))) {
        rust_ui_show_error("Fatal error!", "Failed to read NVS partition.");
        for (;;) {
            vTaskDelay(portMAX_DELAY);
        }
    }
    err = rust_was_init(was_url);
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "failed to initialize Willow Application Server connection");
        rust_ui_show_error("Fatal error!", "WAS initialization failed.");
    }

    if (!rust_config_is_valid()) {
        // wait "indefinitely"
        vTaskDelay(portMAX_DELAY);
    }
}

#ifdef WILLOW_CARGO_FIRST
// esp-idf-sys needs an app_main symbol for its intermediate C link. The final
// Cargo link replaces this weak stub with Rust's strong entry point.
void __attribute__((weak)) app_main(void) {}
#endif
