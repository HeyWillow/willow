#include "board.h"
#include "esp_log.h"

#include "display.h"
#include "rust.h"
#include "system.h"

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
