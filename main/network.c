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
#include "shared.h"
#include "slvgl.h"
#include "system.h"

#define DEFAULT_NTP_CONFIG "Host"
#define DEFAULT_NTP_HOST   "pool.ntp.org"
#define DEFAULT_TIMEZONE   "CST6CDT,M3.2.0,M11.1.0"

#ifndef INET6_ADDRSTRLEN
#define INET6_ADDRSTRLEN 48
#endif

#define HOSTNAME_SIZE 20
#define MAC_ADDR_SIZE 6

#define WIFI_BIT_CONNECTED BIT0

static EventGroupHandle_t hdl_evg;
static const char *TAG = "WILLOW/NETWORK";
uint8_t mac_address[6] = {0x00, 0x11, 0x22, 0x33, 0x44, 0x55};

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

void set_hostname(esp_mac_type_t emt)
{
    esp_err_t ret = ESP_OK;
    uint8_t mac[MAC_ADDR_SIZE];

    ret = esp_read_mac(mac, emt);
    if (ret != ESP_OK) {
        ESP_LOGE(TAG, "failed to read MAC address, using default hostname (%s)", CONFIG_LWIP_LOCAL_HOSTNAME);
        return;
    }

    while (esp_netif_get_nr_of_ifs() == 0) {
        vTaskDelay(100 / portTICK_PERIOD_MS);
    }

    char hostname[HOSTNAME_SIZE];
    hdl_netif = esp_netif_next_unsafe(NULL);

    snprintf(hostname, HOSTNAME_SIZE, "willow-%02x%02x%02x%02x%02x%02x", mac[0], mac[1], mac[2], mac[3], mac[4],
             mac[5]);

    ret = esp_netif_set_hostname(hdl_netif, hostname);
    if (ret != ESP_OK) {
        ESP_LOGE(TAG, "failed to set hostname (%s): %s", hostname, esp_err_to_name(ret));
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

static void hdlr_ev_ip(void *arg, esp_event_base_t ev_base, int32_t ev_id, void *data)
{
    // esp_err_t ret = ESP_OK;

    if (ev_id == IP_EVENT_STA_GOT_IP) {
        ip_event_got_ip_t *ev_ip = (ip_event_got_ip_t *)data;
        ESP_LOGI(TAG, "received IP: " IPSTR, IP2STR(&ev_ip->ip_info.ip));
        xEventGroupSetBits(hdl_evg, WIFI_BIT_CONNECTED);
        return;
    }

    ESP_LOGI(TAG, "unhandled network event ev_base='%s' ev_id='%" PRId32 "'", ev_base, ev_id);
}

static void hdlr_ev_wifi(void *arg, esp_event_base_t ev_base, int32_t ev_id, void *data)
{
    switch (ev_id) {
        case WIFI_EVENT_STA_CONNECTED:
            wifi_event_sta_connected_t *ev_data_connected = (wifi_event_sta_connected_t *)data;
            ESP_LOGI(TAG, "connected to AP (BSSID='" MACSTR "' SSID='%s' channel='%" PRIu8 "')",
                     MAC2STR(ev_data_connected->bssid), ev_data_connected->ssid, ev_data_connected->channel);
            break;

        case WIFI_EVENT_STA_DISCONNECTED:
            wifi_event_sta_disconnected_t *ev_data_disconnected = (wifi_event_sta_disconnected_t *)data;
            ESP_LOGI(TAG, "disconnected from AP (BSSID='" MACSTR "' SSID='%s' reason='%" PRIu8 "' rssi='%'" PRId8 "')",
                     MAC2STR(ev_data_disconnected->bssid), ev_data_disconnected->ssid, ev_data_disconnected->reason,
                     ev_data_disconnected->rssi);
            if (state < STATE_RESTARTING) {
                ESP_LOGI(TAG, "reconnecting");
                esp_wifi_connect();
            }
            break;

        case WIFI_EVENT_STA_START:
            ESP_LOGI(TAG, "WIFI_EVENT_STA_START");
            break;

        default:
            ESP_LOGI(TAG, "unhandled network event ev_base='%s' ev_id='%" PRId32 "'", ev_base, ev_id);
            break;
    }
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

    ret = esp_event_handler_register(IP_EVENT, IP_EVENT_STA_GOT_IP, &hdlr_ev_ip, NULL);
    if (ret != ESP_OK) {
        ESP_LOGE(TAG, "failed to register IP event handler: %s", esp_err_to_name(ret));
        return ret;
    }

    ret = esp_event_handler_register(WIFI_EVENT, ESP_EVENT_ANY_ID, &hdlr_ev_wifi, NULL);
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

    set_hostname(ESP_MAC_WIFI_STA);

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

    vEventGroupDelete(hdl_evg);
    start_sntp();

    ret = esp_wifi_set_ps(WIFI_PS_NONE);
    if (ret != ESP_OK) {
        ESP_LOGE(TAG, "failed to set Wi-Fi power save mode");
    }
    return ret;
}
#endif

void get_mac_address(void)
{
    uint8_t mac[MAC_ADDR_SIZE];
    esp_wifi_get_mac(ESP_IF_WIFI_STA, mac);
    ESP_LOGI(TAG, "MAC address: %02x:%02x:%02x:%02x:%02x:%02x", mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]);
}
