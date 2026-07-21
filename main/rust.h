#pragma once

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#include "esp_err.h"
#include "esp_event_base.h"
#include "esp_mac.h"
#include "esp_netif.h"

bool rust_config_copy_char(const char *key, char *output, size_t output_len);
int8_t rust_config_get_bool(const char *key);
intptr_t rust_config_get_char_len(const char *key);
int64_t rust_config_get_int(const char *key);
bool rust_config_load(void);
bool rust_config_write(const char *data);
void rust_get_mac_address(void);
void rust_ip_event_handler(void *arg, esp_event_base_t event_base, int32_t event_id,
                           void *event_data);
bool rust_nvs_apply(const char *data);
esp_netif_t *rust_set_hostname(esp_mac_type_t mac_type);
esp_err_t rust_sntp_init(uint32_t ip_event_to_renew);
esp_err_t rust_sntp_start(void);
bool rust_state_is_nvs_ok(void);
bool rust_state_is_restarting(void);
void rust_state_mark_nvs_ok(void);
void rust_state_mark_restarting(void);
void rust_wifi_event_handler(void *arg, esp_event_base_t event_base, int32_t event_id,
                             void *event_data);
