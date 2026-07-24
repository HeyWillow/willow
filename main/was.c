#include "cJSON.h"
#include "esp_log.h"
#include "esp_transport_ws.h"
#include "esp_websocket_client.h"

#include "rust.h"
#include "shared.h"
#include "was.h"

static const char *TAG = "WILLOW/WAS";
static struct notify_data *notify_active;

struct notify_data {
    uint64_t id;
    char *audio_url;
    bool backlight;
    bool backlight_max;
    _Atomic bool cancel;
    char *text;
    int repeat;
    int strobe_period_ms;
    int volume;
};

static void notify_task(void *data);

void willow_was_event_handler(
    void *arg_evh, esp_event_base_t base_ev, int32_t id_ev, void *ev_data)
{
    esp_websocket_event_data_t *data = (esp_websocket_event_data_t *)ev_data;
    // components/esp_websocket_client/include/esp_websocket_client.h - enum esp_websocket_event_id_t
    switch (id_ev) {
        case WEBSOCKET_EVENT_CONNECTED:
            ESP_LOGI(TAG, "WebSocket connected");
            rust_was_send_hello();
            if (!rust_config_is_valid()) {
                rust_was_request_config();
            }
            rust_ui_hide_connecting();
            break;
        case WEBSOCKET_EVENT_DATA:
            ESP_LOGV(TAG, "WebSocket data received");
            if (data->op_code == WS_TRANSPORT_OPCODES_TEXT) {
                char *resp = strndup((char *)data->data_ptr, data->data_len);
                ESP_LOGI(TAG, "received text data on WebSocket: %s", resp);
                if (rust_was_handle_control(resp)) {
                    free(resp);
                    break;
                }
                cJSON *cjson = cJSON_Parse(resp);

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
                                xSemaphoreTake(rust_was_notify_mutex(), portMAX_DELAY);
                                if (notify_active == NULL) {
                                    ESP_LOGW(TAG, "trying to cancel notify_task but notify_active is NULL");
                                    xSemaphoreGive(rust_was_notify_mutex());
                                    goto cleanup;
                                }
                                if (notify_active->id == nd->id) {
                                    ESP_LOGI(TAG, "cancel active notify_task with ID='%" PRIu64 "'", nd->id);
                                    notify_active->cancel = true;
                                    xSemaphoreGive(rust_was_notify_mutex());
                                    rust_audio_cancel_playback();
                                    goto cleanup;
                                }
                                xSemaphoreGive(rust_was_notify_mutex());
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
            rust_was_init(was_url);
            break;
        default:
            ESP_LOGD(TAG, "unhandled WebSocket event - ID: %" PRIu32, id_ev);
            break;
    }
}

static void notify_task(void *data)
{
    bool strobe_started = false;
    cJSON *cjson = NULL;
    char *json = NULL;
    esp_err_t ret;
    int i;
    struct notify_data *nd = (struct notify_data *)data;

    if (!nd) {
        ESP_LOGW(TAG, "notify_task called with empty data");
        goto out;
    }

    xSemaphoreTake(rust_was_notify_mutex(), portMAX_DELAY);
    notify_active = nd;
    xSemaphoreGive(rust_was_notify_mutex());

    ESP_LOGI(TAG, "started notify task for notification with ID='%" PRIu64 "'", nd->id);

    rust_ui_show_notification(nd->text);
    free(nd->text);

    rust_display_timer_reset(true);
    rust_backlight_set(nd->backlight, nd->backlight_max);

    if (nd->strobe_period_ms > 0) {
        ret = rust_backlight_strobe_start((uint32_t)nd->strobe_period_ms);
        if (ret == ESP_OK) {
            strobe_started = true;
        } else {
            ESP_LOGE(TAG, "failed to start display backlight strobe: %s", esp_err_to_name(ret));
        }
    }

    if (nd->audio_url != NULL) {
        ret = rust_audio_set_volume(nd->volume);
        if (ret != ESP_OK) {
            ESP_LOGE(TAG, "failed to set notification volume: %s", esp_err_to_name(ret));
        }

        for (i = 0; i < nd->repeat; i++) {
            if (nd->cancel || rust_ui_notification_cancelled()) {
                break;
            }
            rust_audio_play_sync(nd->audio_url);
            vTaskDelay(1000 / portTICK_PERIOD_MS);
        }

        free(nd->audio_url);

        ret = rust_audio_set_volume(-1);
        if (ret != ESP_OK) {
            ESP_LOGE(TAG, "failed to restore configured volume: %s", esp_err_to_name(ret));
        }
    }

    rust_ui_notification_end();

    rust_display_timer_reset(false);

out:
    if (strobe_started) {
        rust_backlight_strobe_stop();
    }

    if (nd->id == 1) {
        goto skip_notify_done;
    }

    if (!rust_was_is_connected(true)) {
        goto skip_notify_done;
    }

    cjson = cJSON_CreateObject();
    if (!cJSON_AddNumberToObject(cjson, "notify_done", nd->id)) {
        goto cleanup;
    }

    json = cJSON_Print(cjson);

    ret = esp_websocket_client_send_text(
        rust_was_client_handle(), json, strlen(json), 2000 / portTICK_PERIOD_MS);
    cJSON_free(json);
    if (ret < 0) {
        ESP_LOGE(TAG, "failed to send WAS notify_done message");
    }

cleanup:
    cJSON_Delete(cjson);

skip_notify_done:
    xSemaphoreTake(rust_was_notify_mutex(), portMAX_DELAY);
    notify_active = NULL;
    xSemaphoreGive(rust_was_notify_mutex());
    free(nd);
    vTaskDelete(NULL);
}
