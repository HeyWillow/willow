#include "esp_log.h"

#ifdef CONFIG_WILLOW_DEBUG_LOG
#define WILLOW_LOG_LEVEL ESP_LOG_DEBUG
#else
#define WILLOW_LOG_LEVEL ESP_LOG_INFO
#endif

void init_logging(void)
{
#ifdef CONFIG_WILLOW_DEBUG_LOG
    esp_log_level_set("*", ESP_LOG_DEBUG);
#else
    esp_log_level_set("*", ESP_LOG_ERROR);
    esp_log_level_set("AUDIO_RECORDER", ESP_LOG_INFO);
#endif

    esp_log_level_set("WILLOW/AUDIO", WILLOW_LOG_LEVEL);
    esp_log_level_set("WILLOW/CONFIG", WILLOW_LOG_LEVEL);
    esp_log_level_set("WILLOW/DISPLAY", WILLOW_LOG_LEVEL);
    esp_log_level_set("WILLOW/ETHERNET", WILLOW_LOG_LEVEL);
    esp_log_level_set("WILLOW/HASS", WILLOW_LOG_LEVEL);
    esp_log_level_set("WILLOW/HTTP", WILLOW_LOG_LEVEL);
    esp_log_level_set("WILLOW/I2C", WILLOW_LOG_LEVEL);
    esp_log_level_set("WILLOW/INPUT", WILLOW_LOG_LEVEL);
    esp_log_level_set("WILLOW/MAIN", WILLOW_LOG_LEVEL);
    esp_log_level_set("WILLOW/NETWORK", WILLOW_LOG_LEVEL);
    esp_log_level_set("WILLOW/OPENHAB", WILLOW_LOG_LEVEL);
    esp_log_level_set("WILLOW/OTA", WILLOW_LOG_LEVEL);
    esp_log_level_set("WILLOW/REST", WILLOW_LOG_LEVEL);
    esp_log_level_set("WILLOW/RUST", WILLOW_LOG_LEVEL);
    esp_log_level_set("WILLOW/SYSTEM", WILLOW_LOG_LEVEL);
    esp_log_level_set("WILLOW/TIMER", WILLOW_LOG_LEVEL);
    esp_log_level_set("WILLOW/UI", WILLOW_LOG_LEVEL);
    esp_log_level_set("WILLOW/WAS", WILLOW_LOG_LEVEL);
}
