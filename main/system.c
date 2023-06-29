#include "esp_log.h"
#include "esp_lvgl_port.h"
#include "esp_random.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"
#include "lvgl.h"
#include "sdkconfig.h"

#include "slvgl.h"
#include "system.h"

static const char *TAG = "WILLOW/SYSTEM";
static const char *wilow_hw_t[WILLOW_HW_MAX] = {
    [WILLOW_HW_UNSUPPORTED] = "Hardware not supported.",
    [WILLOW_HW_ESP32_S3_BOX] = "ESP32-S3-BOX",
    [WILLOW_HW_ESP32_S3_BOX_LITE] = "ESP32-S3-BOX-Lite",
};

const char *str_hw_type(int id)
{
    if (id < 0 || id >= WILLOW_HW_MAX || !wilow_hw_t[id]) {
        return "Invalid hardware type.";
    }
    return wilow_hw_t[id];
}

static void set_hw_type(void)
{
#if defined(CONFIG_ESP32_S3_BOX_BOARD)
    hw_type = WILLOW_HW_ESP32_S3_BOX;
#elif defined(CONFIG_ESP32_S3_BOX_LITE_BOARD)
    hw_type = WILLOW_HW_ESP32_S3_BOX_LITE;
#else
    hw_type = WILLOW_HW_UNSUPPORTED;
#endif
    ESP_LOGD(TAG, "hardware type %d (%s)", hw_type, str_hw_type(hw_type));
}

void init_system(void)
{
    set_hw_type();
}

void restart_delayed(void)
{
    uint32_t delay = esp_random() % 9;
    if (delay < 3) {
        delay = 3;
    } else if (delay > 6) {
        delay = 6;
    }

    ESP_LOGI(TAG, "restarting after %d seconds", delay);

    lvgl_port_lock(0);
    lv_label_set_text_fmt(lbl_ln4, "Restarting in %ds", delay);
    lv_obj_align(lbl_ln4, LV_ALIGN_TOP_MID, 0, 120);
    lv_obj_clear_flag(lbl_ln4, LV_OBJ_FLAG_HIDDEN);
    lvgl_port_unlock();

    delay *= 1000;
    vTaskDelay(delay / portTICK_PERIOD_MS);
    esp_restart();
}
