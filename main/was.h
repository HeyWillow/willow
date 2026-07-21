#include "esp_netif_types.h"

extern esp_netif_t *hdl_netif;
extern char was_url[2048];

void deinit_was(void);
esp_err_t init_was(void);
esp_err_t was_send_endpoint(const char *json, bool nc_skip);
void request_config(void);
void send_wake_start(float wake_volume);
void send_wake_end(void);
