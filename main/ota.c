#include <stdlib.h>

#include "esp_log.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"

#include "audio.h"
#include "rust.h"
#include "was.h"

static const char *TAG = "WILLOW/OTA";

void ota_task(void *data)
{
    char *url = (char *)data;
    bool installed = rust_ota_install(url);

    free(url);

    ESP_LOGI(TAG, "OTA %s, restarting", installed ? "completed" : "failed");
    rust_ui_show_center_message(installed ? "Upgrade Done" : "Upgrade Failed");
    rust_system_restart_delayed();
    vTaskDelete(NULL);
}

void ota_start(char *url)
{
    rust_display_timer_reset(true);
    rust_ui_show_center_message("Starting Upgrade");
    rust_backlight_set(true, false);

    deinit_audio();
    deinit_was();

    vTaskDelay(1000 / portTICK_PERIOD_MS);
    xTaskCreate(&ota_task, "ota_task", 8192, url, 5, NULL);
}
