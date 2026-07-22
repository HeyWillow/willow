#include <stdlib.h>

#include "board.h"
#include "esp_log.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"

#include "display.h"
#include "rust.h"
#include "system.h"

#define MIN_STROBE_PERIOD 20

static const char *TAG = "WILLOW/DISPLAY";

esp_err_t init_display(void)
{
    ESP_LOGD(TAG, "initializing display");

    hdl_lcd = (esp_lcd_panel_handle_t)audio_board_lcd_init(hdl_pset, NULL);
    esp_err_t ret = esp_lcd_panel_disp_on_off(hdl_lcd, true);
    if (ret != ESP_OK) {
        ESP_LOGE(TAG, "failed to turn of display: %s", esp_err_to_name(ret));
        return ret;
    }

    return rust_backlight_init();
}

void display_backlight_strobe_task(void *data)
{
    int period_ms = MIN_STROBE_PERIOD;
    willow_strobe_parms_t *wsp = (willow_strobe_parms_t *)data;

    if (wsp->period_ms >= MIN_STROBE_PERIOD) {
        period_ms = wsp->period_ms;
    }
    // this has the potential to leak if the task is deleted before we reach here
    free(wsp);

    ESP_LOGI(TAG, "starting display backlight strobe effect with period '%d'", period_ms);

    while (true) {
        rust_backlight_set(true, true);
        vTaskDelay(period_ms / portTICK_PERIOD_MS);
        rust_backlight_set(false, false);
        vTaskDelay(period_ms / portTICK_PERIOD_MS);
    }

    vTaskDelete(NULL);
}
