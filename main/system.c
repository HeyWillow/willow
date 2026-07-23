#include "esp_log.h"
#include "sdkconfig.h"

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

void init_system(void)
{
    set_hw_type();
}
