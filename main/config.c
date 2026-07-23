#include <limits.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#include "esp_log.h"

#include "audio.h"
#include "config.h"
#include "rust.h"

static const char *TAG = "WILLOW/CONFIG";

// Rust owns configuration filesystem I/O and the parsed, typed document. The
// getters adapt it to the existing C API while consumers are migrated.

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
    rust_was_deinit();
    deinit_audio();

    if (!rust_config_write(data)) {
        rust_system_restart_delayed();
        return;
    }

    rust_ui_show_center_message("Configuration Updated");
    rust_display_timer_reset(true);
    rust_backlight_set(true, false);
    rust_system_restart_delayed();
}
