#include "esp_http_client.h"

esp_http_client_handle_t init_http_client(void)
{
    esp_http_client_config_t cfg_hc = {
        // either host and path or url should be set
        .url = "http://dummy",
        .user_agent = WILLOW_USER_AGENT,
    };

    return esp_http_client_init(&cfg_hc);
}
