#include "esp_log.h"
#include "esp_lvgl_port.h"
#include "esp_mac.h"
#include "esp_netif.h"
#include "esp_netif_sntp.h"
#include "esp_sntp.h"
#include "esp_wifi.h"
#include "lvgl.h"
#include "lwip/ip_addr.h"
#include "sdkconfig.h"

#include "config.h"
#include "network.h"
#include "rust.h"
#include "shared.h"
#include "slvgl.h"

#define DEFAULT_NTP_CONFIG "Host"
#define DEFAULT_NTP_HOST   "pool.ntp.org"
#define DEFAULT_TIMEZONE   "CST6CDT,M3.2.0,M11.1.0"

#ifndef INET6_ADDRSTRLEN
#define INET6_ADDRSTRLEN 48
#endif

#define WIFI_BIT_CONNECTED BIT0

static EventGroupHandle_t hdl_evg;
static const char *TAG = "WILLOW/NETWORK";

// Rust owns migrated network helpers. C retains event registration, passes its
// event-group handle to the IP callback, and stores the returned netif handle.

void cb_sntp(struct timeval *tv)
{
    for (uint8_t i = 0; i < CONFIG_LWIP_SNTP_MAX_SERVERS; ++i) {
        if (esp_sntp_getservername(i)) {
            ESP_LOGI(TAG, "SNTP client synchronized time to %lld from server %s", tv->tv_sec,
                     esp_sntp_getservername(i));
        } else {
            // we have either IPv4 or IPv6 address, let's print it
            char buff[INET6_ADDRSTRLEN];
            ip_addr_t const *ip = esp_sntp_getserver(i);
            if (ipaddr_ntoa_r(ip, buff, INET6_ADDRSTRLEN) != NULL) {
                ESP_LOGI(TAG, "SNTP client synchronized time to %lld from server %s", tv->tv_sec, buff);
            }
        }
    }
}

static esp_err_t init_sntp(void)
{
    ESP_LOGI(TAG, "initializing SNTP client");
    esp_err_t ret = ESP_OK;
    char *timezone = config_get_char("timezone", DEFAULT_TIMEZONE);
    setenv("TZ", timezone, 1);
    free(timezone);
    tzset();

    esp_sntp_config_t esp_sntp_config = ESP_NETIF_SNTP_DEFAULT_CONFIG_MULTIPLE(0, {});
    esp_sntp_config.sync_cb = cb_sntp;
#ifdef CONFIG_WILLOW_ETHERNET
    esp_sntp_config.ip_event_to_renew = IP_EVENT_ETH_GOT_IP;
#else
    esp_sntp_config.ip_event_to_renew = IP_EVENT_STA_GOT_IP;
#endif
    esp_sntp_config.renew_servers_after_new_IP = true;
    esp_sntp_config.server_from_dhcp = true;
    esp_sntp_config.start = false;
    ret = esp_netif_sntp_init(&esp_sntp_config);

    return ret;
}

static esp_err_t start_sntp(void)
{
    char *ntp_config = config_get_char("ntp_config", DEFAULT_NTP_CONFIG);
    if (strcmp(ntp_config, "DHCP") == 0) {
        ESP_LOGI(TAG, "Using DHCP SNTP server");
        esp_sntp_servermode_dhcp(1);
    } else if (strcmp(ntp_config, "Host") == 0) {
        char *ntp_host = config_get_char("ntp_host", DEFAULT_NTP_HOST);
        ESP_LOGI(TAG, "Using configured SNTP server '%s'", ntp_host);
        esp_sntp_setservername(0, ntp_host);
    }
    free(ntp_config);

    return esp_netif_sntp_start();
}

#ifndef CONFIG_WILLOW_ETHERNET
esp_err_t init_wifi(const char *psk, const char *ssid)
{
    esp_err_t ret = ESP_OK;

    hdl_evg = xEventGroupCreate();

    init_sntp();

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
    if (lvgl_port_lock(lvgl_lock_timeout)) {
        lv_obj_clear_flag(lbl_ln4, LV_OBJ_FLAG_HIDDEN);
        lv_obj_set_style_text_align(lbl_ln4, LV_TEXT_ALIGN_CENTER, 0);
        lv_label_set_text_static(lbl_ln4, "Connecting to Wi-Fi...");
        lvgl_port_unlock();
    }

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

    hdl_netif = rust_set_hostname(ESP_MAC_WIFI_STA);

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
    start_sntp();

    ret = esp_wifi_set_ps(WIFI_PS_NONE);
    if (ret != ESP_OK) {
        ESP_LOGE(TAG, "failed to set Wi-Fi power save mode");
    }
    return ret;
}
#endif
