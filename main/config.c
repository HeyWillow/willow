#include <limits.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "esp_log.h"
#include "esp_spiffs.h"
#include "esp_system.h"

#include "audio.h"
#include "config.h"
#include "rust.h"
#include "shared.h"
#include "was.h"

#define CONFIG_PATH "/spiffs/user/config/willow.json"

static const char *TAG = "WILLOW/CONFIG";

// Rust owns the parsed, typed configuration. These calls only adapt it to the
// existing C getter API while the remaining consumers are migrated.

bool config_get_bool(char *key, const bool default_value)
{
    int8_t value = rust_config_get_bool(key);
    bool ret = value < 0 ? default_value : value != 0;
    ESP_LOGD(TAG, "config_get_bool(%s): %s", key, ret ? "true" : "false");
    return ret;
}

char *config_get_char(const char *key, const char *default_value)
{
    char *ret = NULL;
    intptr_t len = rust_config_get_char_len(key);
    if (len >= 0) {
        ret = calloc((size_t)len + 1, sizeof(char));
        if (ret != NULL && !rust_config_copy_char(key, ret, (size_t)len + 1)) {
            free(ret);
            ret = NULL;
        }
    } else {
        ret = default_value == NULL ? NULL : strndup(default_value, strlen(default_value));
    }
    ESP_LOGD(TAG, "config_get_char(%s)", key);
    return ret;
}

int config_get_int(char *key, const int default_value)
{
    int64_t value = rust_config_get_int(key);
    int ret = value < 0 || value > INT_MAX ? default_value : (int)value;
    ESP_LOGD(TAG, "config_get_int(%s): %d", key, ret);
    return ret;
}

void config_write(const char *data)
{
    deinit_was();
    deinit_audio();

    FILE *f = fopen(CONFIG_PATH, "w");
    if (f == NULL) {
        ESP_LOGE(TAG, "failed to open %s", CONFIG_PATH);
        goto close;
    }
    fputs(data, f);

close:
    fclose(f);

    ESP_LOGI(TAG, "%s updated, restarting", CONFIG_PATH);
    rust_ui_show_center_message("Configuration Updated");
    rust_display_timer_reset(true);
    rust_backlight_set(true, false);
    rust_system_restart_delayed();
}
