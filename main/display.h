#include "periph_lcd.h"

typedef struct {
    int period_ms;
} willow_strobe_parms_t;

extern esp_lcd_panel_handle_t hdl_lcd;

esp_err_t init_display(void);
void display_backlight_strobe_task(void *data);
