#include "esp_netif.h"

extern esp_netif_t *hdl_netif;

esp_err_t init_wifi(const char *psk, const char *ssid);
esp_err_t init_ethernet(void);
esp_err_t init_sntp(void);
esp_err_t start_sntp(void);
