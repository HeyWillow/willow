#pragma once

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#include "esp_err.h"
#include "driver/i2c_master.h"

i2c_master_bus_handle_t rust_i2c_master_handle(void);
bool rust_config_is_valid(void);
esp_err_t rust_network_init(void);
bool rust_nvs_read_was_url(char *output, size_t output_len);
esp_err_t rust_was_init(const char *url);
void rust_was_handle_message(const char *message);
void rust_was_request_config(void);
esp_err_t rust_was_send_endpoint(const char *json);
void rust_was_send_hello(void);
void rust_was_send_wake_end(void);
void rust_was_send_wake_start(float wake_volume);
void rust_ui_hide_connecting(void);
void rust_ui_show_error(const char *primary, const char *secondary);
void rust_ui_show_listening(void);
void rust_ui_show_ready(const char *message);
void rust_ui_show_recognition(const char *heading, const char *body);
void rust_ui_show_thinking(bool multiwake_won);
