#pragma once

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#include "esp_err.h"
#include "esp_websocket_client.h"
#include "driver/i2c_master.h"
#include "freertos/semphr.h"

void rust_audio_cancel_playback(void);
void rust_audio_deinit(void);
void rust_audio_multiwake_result(bool won);
void rust_audio_play_response(bool ok, const char *text);
esp_err_t rust_audio_play_sync(const char *uri);
esp_err_t rust_audio_set_volume(int32_t volume);
i2c_master_bus_handle_t rust_i2c_master_handle(void);
void rust_backlight_set(bool on, bool maximum);
esp_err_t rust_backlight_strobe_start(uint32_t period_ms);
void rust_backlight_strobe_stop(void);
esp_err_t rust_display_timer_reset(bool pause);
bool rust_config_is_valid(void);
void rust_config_write(const char *data);
esp_err_t rust_network_init(void);
bool rust_nvs_apply(const char *data);
bool rust_nvs_read_was_url(char *output, size_t output_len);
bool rust_ota_install(const char *url);
void rust_system_restart_delayed(void);
esp_websocket_client_handle_t rust_was_client_handle(void);
void rust_was_deinit(void);
esp_err_t rust_was_init(const char *url);
bool rust_was_is_connected(bool wait);
SemaphoreHandle_t rust_was_notify_mutex(void);
void rust_was_request_config(void);
esp_err_t rust_was_send_endpoint(const char *json);
void rust_was_send_hello(void);
void rust_was_send_wake_end(void);
void rust_was_send_wake_start(float wake_volume);
void rust_ui_hide_connecting(void);
void rust_ui_notification_end(void);
bool rust_ui_notification_cancelled(void);
void rust_ui_show_center_message(const char *message);
void rust_ui_show_command_result(const char *heading, const char *body);
void rust_ui_show_error(const char *primary, const char *secondary);
void rust_ui_show_listening(void);
void rust_ui_show_notification(const char *message);
void rust_ui_show_ready(const char *message);
void rust_ui_show_recognition(const char *heading, const char *body);
void rust_ui_show_thinking(bool multiwake_won);
