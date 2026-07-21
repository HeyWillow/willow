#include "esp_log.h"
#include "esp_mac.h"
#include "esp_netif.h"
#include "esp_wifi.h"
#include "sdkconfig.h"

#include "network.h"
#include "rust.h"
#include "shared.h"

#define WIFI_BIT_CONNECTED BIT0

static EventGroupHandle_t hdl_evg;
static const char *TAG = "WILLOW/NETWORK";

// Rust owns migrated network helpers. C retains event registration, passes its
// event-group handle to the IP callback, and stores the returned netif handle.

#ifndef CONFIG_WILLOW_ETHERNET
esp_err_t init_wifi(const char *psk, const char *ssid)
{
    esp_err_t ret = ESP_OK;

    hdl_evg = xEventGroupCreate();

    rust_sntp_init(IP_EVENT_STA_GOT_IP);

    esp_netif_t *netif_wifi = esp_netif_create_default_wifi_sta();
    if (netif_wifi == NULL) {
        ESP_LOGE(TAG, "failed to create Wi-Fi STA interface: %s", esp_err_to_name(ret));
        return ESP_FAIL;
    }

    ret = esp_event_handler_register(IP_EVENT, IP_EVENT_STA_GOT_IP, &rust_ip_event_handler, hdl_evg);
    if (ret != ESP_OK) {
        ESP_LOGE(TAG, "failed to register IP event handler: %s", esp_err_to_name(ret));
        return ret;
    }

    ret = esp_event_handler_register(WIFI_EVENT, ESP_EVENT_ANY_ID, &rust_wifi_event_handler, NULL);
    if (ret != ESP_OK) {
        ESP_LOGE(TAG, "failed to register Wi-Fi event handler: %s", esp_err_to_name(ret));
        return ret;
    }

    // Start wifi
    rust_ui_show_connecting("Connecting to Wi-Fi...");

    wifi_init_config_t cfg_wi = WIFI_INIT_CONFIG_DEFAULT();
    ret = esp_wifi_init(&cfg_wi);
    if (ret != ESP_OK) {
        ESP_LOGE(TAG, "failed initialize Wi-Fi: %s", esp_err_to_name(ret));
        return ret;
    }

    wifi_config_t cfg_wifi = {
        .sta = {
            .btm_enabled = 1,
            .failure_retry_cnt = 3,
            .mbo_enabled = 1,
            .rm_enabled = 1,
            .scan_method = WIFI_ALL_CHANNEL_SCAN,
            .sort_method = WIFI_CONNECT_AP_BY_SIGNAL,
            .threshold = {
                .authmode = WIFI_AUTH_WPA2_PSK,
            }
        },
    };

    strlcpy((char *)cfg_wifi.sta.password, psk, sizeof(cfg_wifi.sta.password));
    strlcpy((char *)cfg_wifi.sta.ssid, ssid, sizeof(cfg_wifi.sta.ssid));

    hdl_netif = rust_set_hostname(netif_wifi, ESP_MAC_WIFI_STA);

    ret = esp_wifi_set_config(ESP_IF_WIFI_STA, &cfg_wifi);
    if (ret != ESP_OK) {
        ESP_LOGE(TAG, "failed to set Wi-Fi config: %s", esp_err_to_name(ret));
        return ret;
    }

    ret = esp_wifi_start();
    if (ret != ESP_OK) {
        ESP_LOGE(TAG, "failed to start Wi-Fi: %s", esp_err_to_name(ret));
        return ret;
    }

    ret = esp_wifi_connect();
    if (ret != ESP_OK) {
        ESP_LOGE(TAG, "failed to connect to Wi-Fi: %s", esp_err_to_name(ret));
        return ret;
    }

    EventBits_t evb = xEventGroupWaitBits(hdl_evg, WIFI_BIT_CONNECTED, pdFALSE, pdFALSE, portMAX_DELAY);
    (void)evb;

    // hdl_evg must outlive the registered IP event handler.
    rust_sntp_start();

    ret = esp_wifi_set_ps(WIFI_PS_NONE);
    if (ret != ESP_OK) {
        ESP_LOGE(TAG, "failed to set Wi-Fi power save mode");
    }
    return ret;
}
#endif
