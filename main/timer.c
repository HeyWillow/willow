#include "esp_log.h"
#include "esp_timer.h"
#include "freertos/FreeRTOS.h"
#include "freertos/queue.h"

#include "audio.h"
#include "timer.h"

static const char *TAG = "WILLOW/TIMER";
esp_timer_handle_t hdl_sess_timer = NULL;

static void cb_session_timer(void *data)
{
    if (recording) {
        ESP_LOGI(TAG, "session timer expired - forcing end stream");
        audio_recorder_trigger_stop(hdl_ar);
        int msg = MSG_STOP;
        xQueueSend(q_rec, &msg, 0);
    }
}

esp_err_t init_session_timer(void)
{
    const esp_timer_create_args_t cfg_et = {
        .callback = &cb_session_timer,
        .name = "session_timer",
    };

    return esp_timer_create(&cfg_et, &hdl_sess_timer);
}

esp_err_t reset_timer(esp_timer_handle_t hdl, int timeout, bool pause)
{
    if (esp_timer_is_active(hdl)) {
        esp_timer_stop(hdl);
    }
    if (pause) {
        return ESP_OK;
    }
    return esp_timer_start_once(hdl, timeout * 1000 * 1000);
}
