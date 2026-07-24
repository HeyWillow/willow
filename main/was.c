#include "esp_log.h"
#include "esp_transport_ws.h"
#include "esp_websocket_client.h"

#include "rust.h"
#include "shared.h"
#include "was.h"

static const char *TAG = "WILLOW/WAS";

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
                rust_was_handle_message(resp);
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
