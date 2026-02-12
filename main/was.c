#include "board.h"
#include "cJSON.h"
#include "esp_log.h"
#include "esp_lvgl_port.h"
#include "esp_mac.h"
#include "esp_netif.h"
#include "esp_timer.h"
#include "esp_transport_ws.h"
#include "esp_websocket_client.h"
#include "lvgl.h"
#include "nvs_flash.h"

#include "audio.h"
#include "config.h"
#include "display.h"
#include "network.h"
#include "ota.h"
#include "shared.h"
#include "slvgl.h"
#include "system.h"
#include "timer.h"
#include "ui.h"
#include "was.h"

#define WAS_RECONNECT_TIMEOUT_MS 10 * 1000

static SemaphoreHandle_t notify_mutex;
static const char *TAG = "WILLOW/WAS";
static esp_websocket_client_handle_t hdl_wc = NULL;
static struct notify_data *notify_active;

esp_netif_t *hdl_netif;

struct notify_data {
    uint64_t id;
    char *audio_url;
    bool backlight;
    bool backlight_max;
    bool cancel;
    char *text;
    int repeat;
    int strobe_period_ms;
    int volume;
};

static void notify_task(void *data);
static void send_hello_goodbye(const char *type);

static void IRAM_ATTR cb_ws_event(const void *arg_evh, const esp_event_base_t *base_ev, const int32_t id_ev,
                                  const void *ev_data)
{
    esp_websocket_event_data_t *data = (esp_websocket_event_data_t *)ev_data;
    // components/esp_websocket_client/include/esp_websocket_client.h - enum esp_websocket_event_id_t
    switch (id_ev) {
        case WEBSOCKET_EVENT_CONNECTED:
            ESP_LOGI(TAG, "WebSocket connected");
            send_hello_goodbye("hello");
            if (!config_valid) {
                request_config();
            }
            if (lvgl_port_lock(lvgl_lock_timeout)) {
                lv_obj_add_flag(lbl_ln4, LV_OBJ_FLAG_HIDDEN);
                lv_obj_set_style_text_align(lbl_ln4, LV_TEXT_ALIGN_LEFT, 0);
                lvgl_port_unlock();
            }
            break;
        case WEBSOCKET_EVENT_DATA:
            ESP_LOGV(TAG, "WebSocket data received");
            if (data->op_code == WS_TRANSPORT_OPCODES_TEXT) {
                char *resp = strndup((char *)data->data_ptr, data->data_len);
                ESP_LOGI(TAG, "received text data on WebSocket: %s", resp);
                cJSON *cjson = cJSON_Parse(resp);

                // latency sensitive so handle this first
                cJSON *json_wake_result = cJSON_GetObjectItemCaseSensitive(cjson, "wake_result");
                if (cJSON_IsObject(json_wake_result)) {
                    cJSON *won = cJSON_GetObjectItemCaseSensitive(json_wake_result, "won");
                    if (won != NULL && cJSON_IsBool(won)) {
                        if (cJSON_IsFalse(won)) {
                            ESP_LOGI(TAG, "lost wake race, stopping pipelines");
                            multiwake_won = false;
                            audio_recorder_trigger_stop(hdl_ar);
                            goto cleanup;
                        } else if (config_get_bool("wake_confirmation", DEFAULT_WAKE_CONFIRMATION)) {
                            play_audio_ok(NULL);
                        }
                    }
                    goto cleanup;
                }

                cJSON *json_result = cJSON_GetObjectItemCaseSensitive(cjson, "result");
                if (cJSON_IsObject(json_result)) {
                    cJSON *ok = cJSON_GetObjectItemCaseSensitive(json_result, "ok");
                    if (ok != NULL && cJSON_IsBool(ok)) {
                        cJSON *speech = cJSON_GetObjectItemCaseSensitive(json_result, "speech");
                        if (lvgl_port_lock(lvgl_lock_timeout)) {
                            lv_obj_clear_flag(lbl_ln4, LV_OBJ_FLAG_HIDDEN);
                            lv_obj_clear_flag(lbl_ln5, LV_OBJ_FLAG_HIDDEN);
                            lv_obj_set_style_text_align(lbl_ln4, LV_TEXT_ALIGN_LEFT, 0);
                            lv_obj_set_style_text_align(lbl_ln5, LV_TEXT_ALIGN_LEFT, 0);
                            lv_obj_remove_event_cb(lbl_ln4, cb_btn_cancel);
                            if (cJSON_IsString(speech) && speech->valuestring != NULL
                                && strlen(speech->valuestring) > 0) {
                                cJSON_IsTrue(ok) ? war.fn_ok(speech->valuestring) : war.fn_err(speech->valuestring);
                                lv_label_set_text_static(lbl_ln4, "Response:");
                                lv_label_set_text(lbl_ln5, speech->valuestring);
                            } else {
                                cJSON_IsTrue(ok) ? war.fn_ok("Success") : war.fn_err("Error");
                                lv_label_set_text_static(lbl_ln4, "Command status:");
                                lv_label_set_text(lbl_ln5, cJSON_IsTrue(ok) ? "Success!" : "Error");
                            }
                            lvgl_port_unlock();
                            reset_timer(hdl_display_timer, config_get_int("display_timeout", DEFAULT_DISPLAY_TIMEOUT),
                                        false);
                        }
                    }
                    goto cleanup;
                }

                cJSON *json_config = cJSON_GetObjectItemCaseSensitive(cjson, "config");
                if (cJSON_IsObject(json_config)) {
                    char *config = cJSON_Print(json_config);
                    ESP_LOGI(TAG, "found config in WebSocket message: %s", config);
                    config_write(config);
                    cJSON_free(config);
                    goto cleanup;
                }

                cJSON *json_nvs = cJSON_GetObjectItemCaseSensitive(cjson, "nvs");
                if (cJSON_IsObject(json_nvs)) {
                    char *nvs = cJSON_Print(json_nvs);
                    esp_err_t ret = ESP_OK;
                    nvs_handle_t hdl_nvs;

                    ESP_LOGI(TAG, "found nvs in WebSocket message: %s", nvs);
                    cJSON_free(nvs);

                    cJSON *json_was = cJSON_GetObjectItemCaseSensitive(json_nvs, "WAS");
                    if (cJSON_IsObject(json_was)) {
                        ESP_LOGD(TAG, "found WAS in nvs message");
                        ret = nvs_open("WAS", NVS_READWRITE, &hdl_nvs);
                        if (ret != ESP_OK) {
                            ESP_LOGE(TAG, "failed to open NVS namespace WAS");
                            goto cleanup;
                        }
                        cJSON *json_was_url = cJSON_GetObjectItemCaseSensitive(json_was, "URL");
                        if (cJSON_IsString(json_was_url) && json_was_url->valuestring != NULL) {
                            ESP_LOGD(TAG, "found WAS URL in nvs message");
                            ret = nvs_set_str(hdl_nvs, "URL", json_was_url->valuestring);
                            if (ret != ESP_OK) {
                                ESP_LOGE(TAG, "failed to set URL in NVS namespace WAS");
                                goto cleanup;
                            }
                        }
                        nvs_commit(hdl_nvs);
                    }

                    cJSON *json_wifi = cJSON_GetObjectItemCaseSensitive(json_nvs, "WIFI");
                    if (cJSON_IsObject(json_wifi)) {
                        ESP_LOGD(TAG, "found WIFI in nvs message");
                        ret = nvs_open("WIFI", NVS_READWRITE, &hdl_nvs);
                        if (ret != ESP_OK) {
                            ESP_LOGE(TAG, "failed to open NVS namespace WIFI");
                            goto cleanup;
                        }
                        cJSON *json_wifi_psk = cJSON_GetObjectItemCaseSensitive(json_wifi, "PSK");
                        if (cJSON_IsString(json_wifi_psk) && json_wifi_psk->valuestring != NULL) {
                            ESP_LOGD(TAG, "found WIFI PSK in nvs message");
                            ret = nvs_set_str(hdl_nvs, "PSK", json_wifi_psk->valuestring);
                            if (ret != ESP_OK) {
                                ESP_LOGE(TAG, "failed to set PSK in NVS namespace WIFI");
                                goto cleanup;
                            }
                        }
                        cJSON *json_wifi_ssid = cJSON_GetObjectItemCaseSensitive(json_wifi, "SSID");
                        if (cJSON_IsString(json_wifi_ssid) && json_wifi_ssid->valuestring != NULL) {
                            ESP_LOGD(TAG, "found WIFI SSID in nvs message");
                            ret = nvs_set_str(hdl_nvs, "SSID", json_wifi_ssid->valuestring);
                            if (ret != ESP_OK) {
                                ESP_LOGE(TAG, "failed to set SSID in NVS namespace WIFI");
                                goto cleanup;
                            }
                        }
                        nvs_commit(hdl_nvs);
                    }

                    ESP_LOGI(TAG, "restarting to apply NVS changes");
                    if (lvgl_port_lock(lvgl_lock_timeout)) {
                        lv_label_set_text_static(lbl_ln3, "Connectivity Updated");
                        lv_obj_add_flag(lbl_ln1, LV_OBJ_FLAG_HIDDEN);
                        lv_obj_add_flag(lbl_ln2, LV_OBJ_FLAG_HIDDEN);
                        lv_obj_add_flag(lbl_ln4, LV_OBJ_FLAG_HIDDEN);
                        lv_obj_add_flag(lbl_ln5, LV_OBJ_FLAG_HIDDEN);
                        lv_obj_clear_flag(lbl_ln3, LV_OBJ_FLAG_HIDDEN);
                        lvgl_port_unlock();
                    }
                    reset_timer(hdl_display_timer, config_get_int("display_timeout", DEFAULT_DISPLAY_TIMEOUT), true);
                    display_set_backlight(true, false);
                    deinit_was();
                    restart_delayed();
                }

                cJSON *json_cmd = cJSON_GetObjectItemCaseSensitive(cjson, "cmd");
                if (cJSON_IsString(json_cmd) && json_cmd->valuestring != NULL) {
                    ESP_LOGI(TAG, "found command in WebSocket message: %s", json_cmd->valuestring);
                    if (strcmp(json_cmd->valuestring, "notify") == 0) {
                        ESP_LOGI(TAG, "received notify command");
                        cJSON *data = cJSON_GetObjectItemCaseSensitive(cjson, "data");
                        if (cJSON_IsObject(data)) {
                            struct notify_data *nd = (struct notify_data *)calloc(1, sizeof(struct notify_data));

                            cJSON *id = cJSON_GetObjectItemCaseSensitive(data, "id");
                            if (cJSON_IsNumber(id)) {
                                nd->id = id->valuedouble;
                            } else {
                                ESP_LOGW(TAG, "ignoring notification without ID");
                                goto cleanup;
                            }

                            cJSON *cancel = cJSON_GetObjectItemCaseSensitive(data, "cancel");
                            if (cJSON_IsBool(cancel) && cJSON_IsTrue(cancel)) {
                                xSemaphoreTake(notify_mutex, portMAX_DELAY);
                                if (notify_active == NULL) {
                                    ESP_LOGW(TAG, "trying to cancel notify_task but notify_active is NULL");
                                    xSemaphoreGive(notify_mutex);
                                    goto cleanup;
                                }
                                if (notify_active->id == nd->id) {
                                    ESP_LOGI(TAG, "cancel active notify_task with ID='%" PRIu64 "'", nd->id);
                                    notify_active->cancel = true;
                                    xSemaphoreGive(notify_mutex);
                                    esp_audio_stop(hdl_ea, TERMINATION_TYPE_NOW);
                                    goto cleanup;
                                }
                                xSemaphoreGive(notify_mutex);
                            }

                            cJSON *audio_url = cJSON_GetObjectItemCaseSensitive(data, "audio_url");
                            if (cJSON_IsString(audio_url) && audio_url->valuestring != NULL) {
                                ESP_LOGI(TAG, "audio URL in notify command: %s", audio_url->valuestring);
                                nd->audio_url = strndup(audio_url->valuestring, strlen(audio_url->valuestring));
                            } else {
                                nd->audio_url = NULL;
                            }

                            cJSON *text = cJSON_GetObjectItemCaseSensitive(data, "text");
                            if (cJSON_IsString(text) && text->valuestring != NULL) {
                                ESP_LOGI(TAG, "text in notify command: %s", text->valuestring);
                                nd->text = strndup(text->valuestring, strlen(text->valuestring));
                            } else {
                                nd->text = NULL;
                            }

                            cJSON *repeat = cJSON_GetObjectItemCaseSensitive(data, "repeat");
                            if (cJSON_IsNumber(repeat)) {
                                nd->repeat = repeat->valueint;
                            } else {
                                nd->repeat = 1;
                            }

                            cJSON *backlight = cJSON_GetObjectItemCaseSensitive(data, "backlight");
                            if (cJSON_IsBool(backlight)) {
                                nd->backlight = cJSON_IsTrue(backlight) ? true : false;
                            } else {
                                nd->backlight = true;
                            }

                            cJSON *backlight_max = cJSON_GetObjectItemCaseSensitive(data, "backlight_max");
                            if (cJSON_IsBool(backlight_max)) {
                                nd->backlight_max = cJSON_IsTrue(backlight_max) ? true : false;
                            } else {
                                nd->backlight_max = true;
                            }

                            cJSON *strobe_period_ms = cJSON_GetObjectItemCaseSensitive(data, "strobe_period_ms");
                            if (cJSON_IsNumber(strobe_period_ms)) {
                                nd->strobe_period_ms = strobe_period_ms->valueint;
                            } else {
                                nd->strobe_period_ms = 0;
                            }

                            cJSON *volume = cJSON_GetObjectItemCaseSensitive(data, "volume");
                            if (cJSON_IsNumber(volume)) {
                                nd->volume = volume->valueint;
                            } else {
                                nd->volume = 90;
                            }

                            xTaskCreatePinnedToCore(&notify_task, "notify_task", 4096, nd, 4, NULL, 0);
                        }

                        goto cleanup;
                    }

                    if (strcmp(json_cmd->valuestring, "identify") == 0) {
                        ESP_LOGI(TAG, "received identify command");
                        struct notify_data *nd = (struct notify_data *)calloc(1, sizeof(struct notify_data));
                        const char *audio_url = "spiffs://spiffs/user/audio/success.wav";
                        const char *text = "WAS Locate Active!";
                        nd->audio_url = strndup(audio_url, strlen(audio_url));
                        nd->backlight = true;
                        nd->backlight_max = true;
                        nd->id = 1;
                        nd->repeat = 5;
                        nd->text = strndup(text, strlen(text));
                        nd->volume = 90;
                        xTaskCreatePinnedToCore(&notify_task, "notify_task", 4096, nd, 4, NULL, 0);
                        goto cleanup;
                    }

                    if (strcmp(json_cmd->valuestring, "ota_start") == 0) {
                        cJSON *json_ota_url = cJSON_GetObjectItemCaseSensitive(cjson, "ota_url");
                        if (cJSON_IsString(json_ota_url) && json_ota_url->valuestring != NULL) {
                            // we can't pass json_ota_url->valuestring to ota_start
                            // it will be freed before the OTA task reads it
                            char *ota_url = strndup(json_ota_url->valuestring, (strlen(json_ota_url->valuestring)));
                            ESP_LOGI(TAG, "OTA URL: %s", ota_url);
                            ota_start(ota_url);
                        }
                        goto cleanup;
                    }

                    if (strcmp(json_cmd->valuestring, "restart") == 0) {
                        ESP_LOGI(TAG, "restart command received. restart");
                        if (lvgl_port_lock(lvgl_lock_timeout)) {
                            lv_label_set_text_static(lbl_ln3, "WAS Restart");
                            lv_obj_add_flag(lbl_ln1, LV_OBJ_FLAG_HIDDEN);
                            lv_obj_add_flag(lbl_ln2, LV_OBJ_FLAG_HIDDEN);
                            lv_obj_add_flag(lbl_ln4, LV_OBJ_FLAG_HIDDEN);
                            lv_obj_add_flag(lbl_ln5, LV_OBJ_FLAG_HIDDEN);
                            lv_obj_clear_flag(lbl_ln3, LV_OBJ_FLAG_HIDDEN);
                            lvgl_port_unlock();
                        }
                        display_set_backlight(true, false);
                        deinit_was();
                        restart_delayed();
                    }
                }

cleanup:
                cJSON_Delete(cjson);
                free(resp);
            }
            break;
        case WEBSOCKET_EVENT_DISCONNECTED:
            ESP_LOGI(TAG, "WebSocket disconnected");
            break;
        case WEBSOCKET_EVENT_CLOSED:
            ESP_LOGI(TAG, "WebSocket closed");
            init_was();
            break;
        default:
            ESP_LOGD(TAG, "unhandled WebSocket event - ID: %" PRIu32, id_ev);
            break;
    }
}

void was_deinit_task(void *data)
{
    esp_err_t ret = ESP_OK;
    ESP_LOGI(TAG, "stopping WebSocket client");

    ret = esp_websocket_client_close(hdl_wc, 5000 / portTICK_PERIOD_MS);
    if (ret != ESP_OK) {
        ESP_LOGE(TAG, "failed to cleanly close WebSocket client");

        ret = esp_websocket_client_stop(hdl_wc);
        if (ret != ESP_OK) {
            ESP_LOGE(TAG, "failed to stop WebSocket client: %s", esp_err_to_name(ret));
        }
    }

    vTaskDelete(NULL);
}

void deinit_was(void)
{
    restarting = true;
    send_hello_goodbye("goodbye");
    // needs to be done in a task to avoid this error:
    // WEBSOCKET_CLIENT: Client cannot be stopped from websocket task
    xTaskCreate(&was_deinit_task, "was_deinit_task", 4096, NULL, 5, NULL);
    ESP_LOGI(TAG, "Delay for was_deinit_task");
    vTaskDelay(2000 / portTICK_PERIOD_MS);
}

esp_err_t init_was(void)
{
    if (restarting) {
        return ESP_OK;
    }

    if (notify_mutex == NULL) {
        notify_mutex = xSemaphoreCreateMutex();
    }

    const esp_websocket_client_config_t cfg_wc = {
        .buffer_size = 4096,
        .reconnect_timeout_ms = WAS_RECONNECT_TIMEOUT_MS,
        .task_stack = 6 * 1024, // default 4 * 1024
        .uri = was_url,
        .user_agent = WILLOW_USER_AGENT,
    };
    esp_err_t err = ESP_OK;

    if (lvgl_port_lock(lvgl_lock_timeout)) {
        lv_obj_clear_flag(lbl_ln4, LV_OBJ_FLAG_HIDDEN);
        lv_obj_set_style_text_align(lbl_ln4, LV_TEXT_ALIGN_CENTER, 0);
        lv_label_set_text_static(lbl_ln4, "Connecting to WAS...");
        lvgl_port_unlock();
    }

    esp_log_level_set(TAG, ESP_LOG_DEBUG);
    ESP_LOGI(TAG, "initializing WebSocket client (%s)", was_url);

    hdl_wc = esp_websocket_client_init(&cfg_wc);

    err = esp_websocket_client_destroy_on_exit(hdl_wc);
    if (err != ESP_OK) {
        ESP_LOGW(TAG, "failed to enable destroy on exit: %s", esp_err_to_name(err));
    }

    esp_websocket_register_events(hdl_wc, WEBSOCKET_EVENT_ANY, (esp_event_handler_t)cb_ws_event, NULL);
    err = esp_websocket_client_start(hdl_wc);

    if (err != ESP_OK) {
        ESP_LOGE(TAG, "failed to start WebSocket client: %s", esp_err_to_name(err));
    }
    return err;
}

static bool was_is_connected(const bool wait)
{
    if (esp_websocket_client_is_connected(hdl_wc)) {
        return true;
    }

    if (wait) {
        int max = WAS_RECONNECT_TIMEOUT_MS / 1000;
        for (int i = 0; i < max; i++) {
            if (esp_websocket_client_is_connected(hdl_wc)) {
                return true;
            }
            i++;
        }
        ui_pr_err("WAS disconnected", NULL);
        return false;
    } else {
        return false;
    }
}

esp_err_t was_send_endpoint(const char *data, bool nc_skip)
{
    cJSON *in = NULL, *out = NULL;
    char *json = NULL;
    esp_err_t ret = ESP_OK;

    if (!was_is_connected(true)) {
        if (nc_skip) {
            return ENOTCONN;
        }
    }

    in = cJSON_Parse(data);
    if (!cJSON_IsObject(in)) {
        goto cleanup;
    }

    out = cJSON_CreateObject();
    if (cJSON_AddStringToObject(out, "cmd", "endpoint") == NULL) {
        ret = ESP_FAIL;
        goto cleanup;
    }

    if (!cJSON_AddItemToObjectCS(out, "data", in)) {
        ret = ESP_FAIL;
        goto cleanup;
    }

    json = cJSON_Print(out);

    cJSON_free(out);

    ret = esp_websocket_client_send_text(hdl_wc, json, strlen(json), 2000 / portTICK_PERIOD_MS);
    cJSON_free(json);
    if (ret < 0) {
        ESP_LOGE(TAG, "failed to send message to WAS");
    }
cleanup:
    cJSON_Delete(in);
    return ret;
}

void request_config(void)
{
    cJSON *cjson = NULL;
    char *json = NULL;
    esp_err_t ret;

    // not sure if we should wait here as we call request_config on WEBSOCKET_EVENT_CONNECTED
    if (!was_is_connected(true)) {
        return;
    }

    cjson = cJSON_CreateObject();
    if (cJSON_AddStringToObject(cjson, "cmd", "get_config") == NULL) {
        goto cleanup;
    }

    json = cJSON_Print(cjson);

    ret = esp_websocket_client_send_text(hdl_wc, json, strlen(json), 2000 / portTICK_PERIOD_MS);
    cJSON_free(json);
    if (ret < 0) {
        ESP_LOGE(TAG, "failed to send WAS get_config message");
    }

cleanup:
    cJSON_Delete(cjson);
}

static void send_hello_goodbye(const char *type)
{
    char *json;
    const char *hostname;
    uint8_t mac[6];
    esp_err_t ret;

    ESP_LOGI(TAG, "sending WAS %s", type);

    if (!was_is_connected(true)) {
        return;
    }

    ret = esp_netif_get_hostname(hdl_netif, &hostname);
    if (ret != ESP_OK) {
        ESP_LOGE(TAG, "failed to get hostname");
        return;
    }

    ret = esp_efuse_mac_get_default(mac);
    if (ret != ESP_OK) {
        ESP_LOGE(TAG, "failed to get MAC address from EFUSE");
        return;
    }

    cJSON *cjson = cJSON_CreateObject();
    cJSON *msg = cJSON_CreateObject();
    cJSON *mac_arr = cJSON_CreateArray();

    for (int i = 0; i < 6; i++) {
        cJSON_AddItemToArray(mac_arr, cJSON_CreateNumber(mac[i]));
    }

    if (cJSON_AddStringToObject(msg, "hostname", hostname) == NULL) {
        goto cleanup;
    }
    if (cJSON_AddStringToObject(msg, "hw_type", str_hw_type(hw_type)) == NULL) {
        goto cleanup;
    }
    if (!cJSON_AddItemToObjectCS(msg, "mac_addr", mac_arr)) {
        goto cleanup;
    }
    if (!cJSON_AddItemToObjectCS(cjson, type, msg)) {
        goto cleanup;
    }

    json = cJSON_Print(cjson);

    ret = esp_websocket_client_send_text(hdl_wc, json, strlen(json), 2000 / portTICK_PERIOD_MS);
    cJSON_free(json);
    if (ret < 0) {
        ESP_LOGE(TAG, "failed to send WAS %s message", type);
    }

cleanup:
    cJSON_Delete(cjson);
}

void IRAM_ATTR send_wake_start(float wake_volume)
{
    char *json;
    esp_err_t ret;

    // Silently return if multiwake is not enabled - defaults to not enabled
    if (!config_get_bool("multiwake", false)) {
        return;
    }

    if (!was_is_connected(false)) {
        ESP_LOGW(TAG, "Websocket not connected - skipping wake start");
        return;
    }

    cJSON *cjson = cJSON_CreateObject();
    cJSON *wake_start = cJSON_CreateObject();

    if (!cJSON_AddNumberToObject(wake_start, "wake_volume", wake_volume)) {
        goto cleanup;
    }
    if (!cJSON_AddItemToObjectCS(cjson, "wake_start", wake_start)) {
        goto cleanup;
    }

    json = cJSON_Print(cjson);

    ret = esp_websocket_client_send_text(hdl_wc, json, strlen(json), 2000 / portTICK_PERIOD_MS);
    cJSON_free(json);
    if (ret < 0) {
        ESP_LOGE(TAG, "failed to send WAS wake_start message");
    }

cleanup:
    cJSON_Delete(cjson);
}

void send_wake_end(void)
{
    char *json;
    esp_err_t ret;

    // Silently return if multiwake is not enabled - defaults to not enabled
    if (!config_get_bool("multiwake", false)) {
        return;
    }

    if (!was_is_connected(false)) {
        ESP_LOGW(TAG, "Websocket not connected - skipping wake end");
        return;
    }

    cJSON *cjson = cJSON_CreateObject();
    cJSON *wake_end = cJSON_CreateObject();

    if (!cJSON_AddItemToObjectCS(cjson, "wake_end", wake_end)) {
        goto cleanup;
    }

    json = cJSON_Print(cjson);

    ret = esp_websocket_client_send_text(hdl_wc, json, strlen(json), 2000 / portTICK_PERIOD_MS);
    cJSON_free(json);
    if (ret < 0) {
        ESP_LOGE(TAG, "failed to send WAS wake_end message");
    }

cleanup:
    cJSON_Delete(cjson);
}

void cb_btn_cancel_notify(lv_event_t *ev)
{
    ESP_LOGD(TAG, "btn_cancel pressed");
    esp_audio_stop(hdl_ea, TERMINATION_TYPE_NOW);
    xSemaphoreTake(notify_mutex, portMAX_DELAY);
    notify_active->cancel = true;
    xSemaphoreGive(notify_mutex);
}

static void notify_task(void *data)
{
    TaskHandle_t hdl_task_strobe = NULL;
    cJSON *cjson = NULL;
    char *json = NULL;
    esp_err_t ret;
    int i;
    struct notify_data *nd = (struct notify_data *)data;

    if (!nd) {
        ESP_LOGW(TAG, "notify_task called with empty data");
        goto out;
    }

    xSemaphoreTake(notify_mutex, portMAX_DELAY);
    notify_active = nd;
    xSemaphoreGive(notify_mutex);

    ESP_LOGI(TAG, "started notify task for notification with ID='%" PRIu64 "'", nd->id);

    if (lvgl_port_lock(lvgl_lock_timeout)) {
        if (nd->text == NULL) {
            lv_label_set_text_static(lbl_ln3, "Notification Active");
        } else {
            lv_label_set_text(lbl_ln3, nd->text);
            free(nd->text);
        }
        lv_obj_remove_event_cb(btn_cancel, cb_btn_cancel);
        lv_obj_add_flag(lbl_ln1, LV_OBJ_FLAG_HIDDEN);
        lv_obj_add_flag(lbl_ln2, LV_OBJ_FLAG_HIDDEN);
        lv_obj_add_flag(lbl_ln4, LV_OBJ_FLAG_HIDDEN);
        lv_obj_add_flag(lbl_ln5, LV_OBJ_FLAG_HIDDEN);
        lv_obj_add_event_cb(btn_cancel, cb_btn_cancel_notify, LV_EVENT_PRESSED, NULL);
        lv_obj_clear_flag(lbl_ln3, LV_OBJ_FLAG_HIDDEN);
        lv_obj_clear_flag(btn_cancel, LV_OBJ_FLAG_HIDDEN);
        lv_label_set_long_mode(lbl_ln3, LV_LABEL_LONG_SCROLL);
        lvgl_port_unlock();
    }

    reset_timer(hdl_display_timer, config_get_int("display_timeout", DEFAULT_DISPLAY_TIMEOUT), true);
    display_set_backlight(nd->backlight, nd->backlight_max);

    if (nd->strobe_period_ms > 0) {
        willow_strobe_parms_t *wsp = (willow_strobe_parms_t *)calloc(1, sizeof(willow_strobe_parms_t));
        wsp->period_ms = nd->strobe_period_ms;
        xTaskCreatePinnedToCore(display_backlight_strobe_task, "strobe_task", 2048, wsp, 5, &hdl_task_strobe, 0);
    }

    if (nd->audio_url != NULL) {
        if (hdl_ea == NULL) {
            ESP_LOGW(TAG, "notify with hdl_ea=NULL, skip audio playback");
        } else {
            volume_set(nd->volume);
            gpio_set_level(get_pa_enable_gpio(), 1);
        }

        for (i = 0; i < nd->repeat; i++) {
            if (nd->cancel) {
                break;
            }
            if (hdl_ea != NULL) {
                esp_audio_sync_play(hdl_ea, nd->audio_url, 0);
            }
            vTaskDelay(1000 / portTICK_PERIOD_MS);
        }

        free(nd->audio_url);

        if (hdl_ea != NULL) {
            gpio_set_level(get_pa_enable_gpio(), 0);
            volume_set(-1);
        }
    }

    if (lvgl_port_lock(lvgl_lock_timeout)) {
        lv_obj_add_flag(btn_cancel, LV_OBJ_FLAG_HIDDEN);
        lv_obj_remove_event_cb(btn_cancel, cb_btn_cancel_notify);
        lvgl_port_unlock();
    }

    reset_timer(hdl_display_timer, config_get_int("display_timeout", DEFAULT_DISPLAY_TIMEOUT), false);

out:
    if (hdl_task_strobe != NULL) {
        vTaskDelete(hdl_task_strobe);
        display_set_backlight(true, false);
    }

    if (nd->id == 1) {
        goto skip_notify_done;
    }

    if (!was_is_connected(true)) {
        goto skip_notify_done;
    }

    cjson = cJSON_CreateObject();
    if (!cJSON_AddNumberToObject(cjson, "notify_done", nd->id)) {
        goto cleanup;
    }

    json = cJSON_Print(cjson);

    ret = esp_websocket_client_send_text(hdl_wc, json, strlen(json), 2000 / portTICK_PERIOD_MS);
    cJSON_free(json);
    if (ret < 0) {
        ESP_LOGE(TAG, "failed to send WAS notify_done message");
    }

cleanup:
    cJSON_Delete(cjson);

skip_notify_done:
    xSemaphoreTake(notify_mutex, portMAX_DELAY);
    notify_active = NULL;
    xSemaphoreGive(notify_mutex);
    free(nd);
    vTaskDelete(NULL);
}
