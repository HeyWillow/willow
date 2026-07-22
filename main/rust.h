#pragma once

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#include "esp_err.h"
#include "driver/i2c_master.h"
#include "freertos/queue.h"

QueueHandle_t rust_audio_recorder_queue_handle(void);
i2c_master_bus_handle_t rust_i2c_master_handle(void);
esp_err_t rust_i2c_probe(uint16_t address);
bool rust_config_copy_char(const char *key, char *output, size_t output_len);
int8_t rust_config_get_bool(const char *key);
intptr_t rust_config_get_char_len(const char *key);
int64_t rust_config_get_int(const char *key);
bool rust_config_load(void);
