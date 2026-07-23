#include "esp_mac.h"
#include "esp_netif.h"

extern esp_netif_t *hdl_netif;

esp_err_t init_wifi(const char *psk, const char *ssid);
esp_err_t init_ethernet(void);
void get_mac_address(void);
void set_hostname(esp_netif_t *netif, esp_mac_type_t mac_type);
