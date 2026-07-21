#pragma once

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

bool rust_config_copy_char(const char *key, char *output, size_t output_len);
int8_t rust_config_get_bool(const char *key);
intptr_t rust_config_get_char_len(const char *key);
int64_t rust_config_get_int(const char *key);
bool rust_config_load(void);
bool rust_config_write(const char *data);
