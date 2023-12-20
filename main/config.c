#include <errno.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>

#include "esp_log.h"
#include "esp_lvgl_port.h"
#include "esp_spiffs.h"
#include "esp_system.h"
#include "esp_timer.h"
#include "lvgl.h"

#include "audio.h"
#include "config.h"
#include "display.h"
#include "endpoint/hass.h"
#include "shared.h"
#include "slvgl.h"
#include "system.h"
#include "timer.h"
#include "was.h"

#define CONFIG_PATH "/spiffs/user/config/willow.json"

static const char *TAG = "WILLOW/CONFIG";

bool config_valid = false;
cJSON *wc = NULL; // TODO: cJSON_Delete() after all config_get_* calls

static char *config_read(void)
{
    char *config = NULL;

    struct stat fs;
    if (stat(CONFIG_PATH, &fs)) {
        if (errno == ENOENT) {
            ESP_LOGI(TAG, "%s does not exist, will be requested from WAS", CONFIG_PATH);
        } else {
            ESP_LOGE(TAG, "failed to get file status for %s: %s", CONFIG_PATH, strerror(errno));
        }
        return NULL;
    }

    ESP_LOGI(TAG, "opening %s", CONFIG_PATH);
    FILE *f = fopen(CONFIG_PATH, "r");
    if (f == NULL) {
        ESP_LOGE(TAG, "failed to open %s", CONFIG_PATH);
        goto close;
    }

    ESP_LOGI(TAG, "config file size: %ld", fs.st_size);

    config = calloc(fs.st_size + 1, sizeof(char));
    size_t rlen = fread(config, 1, fs.st_size, f);
    ESP_LOGI(TAG, "fread: %d", rlen);
    config[fs.st_size] = '\0';
    ESP_LOGI(TAG, "config file content: %s", config);
close:
    fclose(f);

    return config;
}

bool config_get_bool(char *key, const bool default_value)
{
    bool ret = default_value;
    cJSON *val = cJSON_GetObjectItemCaseSensitive(wc, key);
    if (val != NULL && cJSON_IsBool(val)) {
        ret = cJSON_IsTrue(val) ? true : false;
    } else {
        ret = default_value;
    }
    ESP_LOGD(TAG, "config_get_bool(%s): %s", key, ret ? "true" : "false");
    return ret;
}

char *config_get_char(const char *key, const char *default_value)
{
    char *ret = NULL;
    cJSON *val = cJSON_GetObjectItemCaseSensitive(wc, key);
    if (val != NULL && cJSON_IsString(val) && val->valuestring != NULL) {
        ret = strndup(val->valuestring, strlen(val->valuestring));
    } else {
        ret = default_value == NULL ? NULL : strndup(default_value, strlen(default_value));
    }
    ESP_LOGD(TAG, "config_get_char(%s): %s", key, ret);
    return ret;
}

int config_get_int(char *key, const int default_value)
{
    int ret = -1;
    cJSON *val = cJSON_GetObjectItemCaseSensitive(wc, key);
    if (cJSON_IsNumber(val)) {
        ret = val->valueint;
    } else {
        ret = default_value;
    }
    ESP_LOGD(TAG, "config_get_int(%s): %d", key, ret);
    return ret;
}

void config_parse(void)
{
    char *config = config_read();
    char *json = NULL;

    if (config == NULL) {
        return;
    }

    wc = cJSON_Parse(config);
    if (wc == NULL) {
        const char *eptr = cJSON_GetErrorPtr();
        if (eptr != NULL) {
            ESP_LOGE(TAG, "error parsing config file: %s\n", eptr);
            goto cleanup;
        }
    }

    config_valid = true;

    json = cJSON_Print(wc);
    ESP_LOGI(TAG, "parsed config file:");
    printf("%s\n", json);
    cJSON_free(json);

cleanup:
    free(config);
}

void config_write(const char *data)
{
    if (state < STATE_NEED_CONFIG) {
        deinit_audio();
        deinit_hass();
    }

    state = STATE_WRITE_FLASH;

    FILE *f = fopen(CONFIG_PATH, "w");
    if (f == NULL) {
        ESP_LOGE(TAG, "failed to open %s", CONFIG_PATH);
        goto close;
    }
    fputs(data, f);

close:
    fclose(f);

    ESP_LOGI(TAG, "%s updated, restarting", CONFIG_PATH);
    if (lvgl_port_lock(lvgl_lock_timeout)) {
        lv_label_set_text_static(lbl_ln3, "Configuration Updated");
        lv_obj_add_flag(lbl_ln1, LV_OBJ_FLAG_HIDDEN);
        lv_obj_add_flag(lbl_ln2, LV_OBJ_FLAG_HIDDEN);
        lv_obj_add_flag(lbl_ln4, LV_OBJ_FLAG_HIDDEN);
        lv_obj_add_flag(lbl_ln5, LV_OBJ_FLAG_HIDDEN);
        lv_obj_clear_flag(lbl_ln3, LV_OBJ_FLAG_HIDDEN);
        lvgl_port_unlock();
    }
    reset_timer(hdl_display_timer, config_get_int("display_timeout", DEFAULT_DISPLAY_TIMEOUT), true);
    display_set_backlight(true, false);
    restart_delayed();
}
