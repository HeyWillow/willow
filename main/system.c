#include <stdio.h>

#include "esp_event.h"
#include "esp_log.h"
#include "esp_random.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"
#include "sdkconfig.h"

#include "rust.h"
#include "shared.h"
#include "system.h"

static const char *TAG = "WILLOW/SYSTEM";
enum willow_hw_t hw_type;

static const char *willow_hw_t[WILLOW_HW_MAX] = {
    [WILLOW_HW_UNSUPPORTED] = "HW-UNSUPPORTED",
    [WILLOW_HW_ESP32_S3_BOX] = "ESP32-S3-BOX",
    [WILLOW_HW_ESP32_S3_BOX_LITE] = "ESP32-S3-BOX-Lite",
    [WILLOW_HW_ESP32_S3_BOX_3] = "ESP32-S3-BOX-3",
};

const char *str_hw_type(int id)
{
    if (id < 0 || id >= WILLOW_HW_MAX || !willow_hw_t[id]) {
        return "Invalid hardware type.";
    }
    return willow_hw_t[id];
}

static void set_hw_type(void)
{
#if defined(CONFIG_ESP32_S3_BOX_BOARD)
    hw_type = WILLOW_HW_ESP32_S3_BOX;
#elif defined(CONFIG_ESP32_S3_BOX_LITE_BOARD)
    hw_type = WILLOW_HW_ESP32_S3_BOX_LITE;
#elif defined(CONFIG_ESP32_S3_BOX_3_BOARD)
    hw_type = WILLOW_HW_ESP32_S3_BOX_3;
#else
    hw_type = WILLOW_HW_UNSUPPORTED;
#endif
    ESP_LOGD(TAG, "hardware type %d (%s)", hw_type, str_hw_type(hw_type));
}

static esp_err_t init_ev_loop()
{
    esp_err_t ret = esp_event_loop_create_default();
    if (ret != ESP_OK) {
        ESP_LOGE(TAG, "failed to initialize default event loop: %s", esp_err_to_name(ret));
    }
    return ret;
}

void init_system(void)
{
    set_hw_type();
    ESP_ERROR_CHECK(init_ev_loop());
}

void restart_delayed(void)
{
    uint32_t delay = esp_random() % 9;
    if (delay < 3) {
        delay = 3;
    } else if (delay > 6) {
        delay = 6;
    }

    ESP_LOGI(TAG, "restarting after %" PRIu32 " seconds", delay);

    char message[32];
    snprintf(message, sizeof(message), "Restarting in %" PRIu32 " seconds", delay);
    rust_ui_show_connecting(message);

    delay *= 1000;
    vTaskDelay(delay / portTICK_PERIOD_MS);
    esp_restart();
}
