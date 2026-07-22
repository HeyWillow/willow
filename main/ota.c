#include <stdlib.h>

#include "esp_log.h"
#include "esp_lvgl_port.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"
#include "lvgl.h"

#include "audio.h"
#include "rust.h"
#include "shared.h"
#include "slvgl.h"
#include "system.h"
#include "was.h"

static const char *TAG = "WILLOW/OTA";

void ota_task(void *data)
{
    char *url = (char *)data;
    bool installed = rust_ota_install(url);

    free(url);

    ESP_LOGI(TAG, "OTA %s, restarting", installed ? "completed" : "failed");
    if (lvgl_port_lock(lvgl_lock_timeout)) {
        lv_label_set_text_static(lbl_ln3, installed ? "Upgrade Done" : "Upgrade Failed");
        lvgl_port_unlock();
    }
    restart_delayed();
    vTaskDelete(NULL);
}

void ota_start(char *url)
{
    rust_display_timer_reset(true);
    if (lvgl_port_lock(lvgl_lock_timeout)) {
        lv_obj_add_flag(lbl_ln1, LV_OBJ_FLAG_HIDDEN);
        lv_obj_add_flag(lbl_ln2, LV_OBJ_FLAG_HIDDEN);
        lv_obj_add_flag(lbl_ln4, LV_OBJ_FLAG_HIDDEN);
        lv_obj_add_flag(lbl_ln5, LV_OBJ_FLAG_HIDDEN);
        lv_label_set_text_static(lbl_ln3, "Starting Upgrade");
        lv_obj_clear_flag(lbl_ln3, LV_OBJ_FLAG_HIDDEN);
        lvgl_port_unlock();
    }
    rust_backlight_set(true, false);

    deinit_audio();
    deinit_was();

    vTaskDelay(1000 / portTICK_PERIOD_MS);
    xTaskCreate(&ota_task, "ota_task", 8192, url, 5, NULL);
}
