#pragma once

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#include "esp_err.h"
#include "driver/i2c_master.h"

i2c_master_bus_handle_t rust_i2c_master_handle(void);
esp_err_t rust_was_send_endpoint(const char *json);
void rust_was_send_wake_end(void);
void rust_was_send_wake_start(float wake_volume);
void rust_ui_show_listening(void);
void rust_ui_show_ready(const char *message);
void rust_ui_show_recognition(const char *heading, const char *body);
void rust_ui_show_thinking(bool multiwake_won);
