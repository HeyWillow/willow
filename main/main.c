#include "esp_err.h"
#include "esp_log.h"
#include "esp_ota_ops.h"
#include "esp_timer.h"
#include "sdkconfig.h"

#include "audio.h"
#include "config.h"
#include "main.h"
#include "rust.h"
#include "shared.h"
#include "tasks.h"
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
