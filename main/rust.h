#pragma once

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#include "esp_err.h"
#include "driver/i2c_master.h"
#include "freertos/queue.h"

QueueHandle_t rust_audio_recorder_queue_handle(void);
i2c_master_bus_handle_t rust_i2c_master_handle(void);
void rust_backlight_set(bool on, bool maximum);
esp_err_t rust_backlight_strobe_start(uint32_t period_ms);
void rust_backlight_strobe_stop(void);
esp_err_t rust_display_timer_init(void);
esp_err_t rust_display_timer_reset(bool pause);
esp_err_t rust_display_init(void);
bool rust_input_is_muted(void);
esp_err_t rust_input_monitor_start(int32_t unmute_event);
bool rust_config_copy_char(const char *key, char *output, size_t output_len);
int8_t rust_config_get_bool(const char *key);
intptr_t rust_config_get_char_len(const char *key);
int64_t rust_config_get_int(const char *key);
bool rust_config_load(void);
esp_err_t rust_spiffs_mount(void);
void rust_system_restart_delayed(void);
bool rust_state_is_nvs_ok(void);
bool rust_state_is_restarting(void);
void rust_state_mark_nvs_ok(void);
void rust_state_mark_restarting(void);
esp_err_t rust_ui_init(void);
esp_err_t rust_ui_touch_init(int32_t stop_event);
void rust_ui_hide_connecting(void);
void rust_ui_notification_end(void);
bool rust_ui_notification_cancelled(void);
void rust_ui_show_center_message(const char *message);
void rust_ui_show_command_result(const char *heading, const char *body);
void rust_ui_show_connecting(const char *message);
void rust_ui_show_error(const char *primary, const char *secondary);
void rust_ui_show_listening(void);
void rust_ui_show_notification(const char *message, void *player);
void rust_ui_show_ready(const char *message);
void rust_ui_show_recognition(const char *heading, const char *body);
void rust_ui_show_thinking(bool multiwake_won);
