#pragma once

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#include "esp_err.h"
#include "esp_netif.h"

bool rust_config_copy_char(const char *key, char *output, size_t output_len);
int8_t rust_config_get_bool(const char *key);
intptr_t rust_config_get_char_len(const char *key);
int64_t rust_config_get_int(const char *key);
bool rust_config_load(void);
bool rust_config_write(const char *data);
esp_err_t rust_ethernet_init(void);
void rust_get_mac_address(void);
bool rust_nvs_apply(const char *data);
bool rust_nvs_read_was_url(char *output, size_t output_len);
bool rust_nvs_read_wifi(char *psk, size_t psk_len, char *ssid,
                        size_t ssid_len);
esp_err_t rust_spiffs_mount(void);
bool rust_state_is_nvs_ok(void);
bool rust_state_is_restarting(void);
void rust_state_mark_restarting(void);
esp_err_t rust_wifi_init(const char *psk, const char *ssid,
                         esp_netif_t **network_interface);
