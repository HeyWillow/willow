#include "board.h"
#include "driver/ledc.h"
#include "esp_err.h"
#include "esp_lcd_panel_ops.h"
#include "esp_lcd_touch_gt911.h"
#include "esp_lcd_touch_tt21100.h"
#include "esp_log.h"
#include "esp_lvgl_port.h"
#include "lvgl.h"

#include "audio.h"
#include "config.h"
#include "rust.h"
#include "system.h"

#define DEFAULT_LOCK_TIMEOUT 500

static const char *TAG = "WILLOW/LVGL";

enum esp32_s3_box_touch_t {
    TOUCH_GT911,
    TOUCH_TT21100,
};
int lvgl_lock_timeout;
lv_disp_t *ld;
lv_obj_t *btn_cancel, *lbl_btn_cancel, *lbl_ln1, *lbl_ln2, *lbl_ln3, *lbl_ln4, *lbl_ln5;

static esp_lcd_panel_io_handle_t hdl_touch_io;

void cb_btn_cancel(lv_event_t *ev)
{
    ESP_LOGD(TAG, "btn_cancel pressed");
    q_msg msg = MSG_STOP;
    xQueueSend(q_rec, &msg, 0);
}

void cb_scr(lv_event_t *ev)
{
    // printf("cb_scr\n");
    switch (lv_event_get_code(ev)) {
        case LV_EVENT_RELEASED:
            rust_display_timer_reset(false);
            break;

        case LV_EVENT_PRESSED:
            rust_display_timer_reset(true);
            rust_backlight_set(true, false);
            break;

        default:
            break;
    }
}

esp_err_t init_lvgl_display(void)
{
    esp_err_t ret = ESP_OK;
    lvgl_lock_timeout = config_get_int("lvgl_lock_timeout", DEFAULT_LOCK_TIMEOUT);
    lvgl_port_cfg_t cfg_lp = ESP_LVGL_PORT_INIT_CONFIG();
    cfg_lp.task_affinity = 0;
    ret = lvgl_port_init(&cfg_lp);
    if (ret != ESP_OK) {
        ESP_LOGE(TAG, "failed to initialize LVGL port: %s", esp_err_to_name(ret));
        return ret;
    }

    esp_lcd_panel_io_handle_t hdl_lcd_io = rust_display_io_handle();
    esp_lcd_panel_handle_t hdl_lcd = rust_display_panel_handle();
    if (hdl_lcd_io == NULL || hdl_lcd == NULL) {
        ESP_LOGE(TAG, "failed to borrow Rust display handles");
        return ESP_FAIL;
    }

    ESP_LOGD(TAG, "init_lvgl: display IO handle: %p", hdl_lcd_io);

    const lvgl_port_display_cfg_t cfg_ld = {
        .buffer_size = LCD_H_RES * LCD_V_RES,
        .double_buffer = true,
        // DMA and SPIRAM
        // E (16:37:21.267) LVGL: lvgl_port_add_disp(190): Alloc DMA capable buffer in SPIRAM is not supported!
        .flags = {
            .buff_dma = false,
            .buff_spiram = true,
        },
        .hres = LCD_H_RES,
        .io_handle = hdl_lcd_io,
        .monochrome = false,
        .panel_handle = hdl_lcd,
        .rotation = {
            .mirror_x = LCD_MIRROR_X,
            .mirror_y = LCD_MIRROR_Y,
            .swap_xy = LCD_SWAP_XY,
        },
        .trans_size = LCD_H_RES * LCD_V_RES / 10,
        .vres = LCD_V_RES,
    };

    ld = lvgl_port_add_disp(&cfg_ld);

    return ret;
}

static esp_lcd_panel_io_i2c_config_t cfg_lpiic_gt911(int addr)
{
    esp_lcd_panel_io_i2c_config_t cfg_io_lt = ESP_LCD_TOUCH_IO_I2C_GT911_CONFIG();
    cfg_io_lt.dev_addr = addr;

    return cfg_io_lt;
}

static esp_lcd_panel_io_i2c_config_t cfg_lpiic_tt21100(void)
{
    esp_lcd_panel_io_i2c_config_t cfg_io_lt = ESP_LCD_TOUCH_IO_I2C_TT21100_CONFIG();

    return cfg_io_lt;
}

esp_err_t init_lvgl_touch(void)
{
    enum esp32_s3_box_touch_t touch_type;
    esp_err_t ret = ESP_OK;

    switch (hw_type) {
        case WILLOW_HW_ESP32_S3_BOX:
            __attribute__((fallthrough));
        case WILLOW_HW_ESP32_S3_BOX_3:
            break;
        default:
            ESP_LOGI(TAG, "%s does not have a touch screen, skipping init", str_hw_type(hw_type));
            return ret;
    }

    esp_lcd_touch_config_t cfg_lt = {
        .flags = {
            .mirror_x = false,
            .mirror_y = false,
            .swap_xy = LCD_SWAP_XY,
        },
        .levels = {
            .interrupt = 0,
            .reset = 0,
        },
        .int_gpio_num = GPIO_NUM_3,
        .rst_gpio_num = GPIO_NUM_NC,
        .x_max = LCD_H_RES,
        .y_max = LCD_V_RES,
    };

    esp_lcd_panel_io_i2c_config_t cfg_io_lt;

    if (rust_i2c_probe(ESP_LCD_TOUCH_IO_I2C_GT911_ADDRESS) == ESP_OK) {
        cfg_io_lt = cfg_lpiic_gt911(ESP_LCD_TOUCH_IO_I2C_GT911_ADDRESS);
        touch_type = TOUCH_GT911;
        ESP_LOGI(TAG, "detected GT911 touch controller on address 0x%02x", ESP_LCD_TOUCH_IO_I2C_GT911_ADDRESS);
    } else if (rust_i2c_probe(ESP_LCD_TOUCH_IO_I2C_GT911_ADDRESS_BACKUP) == ESP_OK) {
        cfg_io_lt = cfg_lpiic_gt911(ESP_LCD_TOUCH_IO_I2C_GT911_ADDRESS_BACKUP);
        touch_type = TOUCH_GT911;
        ESP_LOGI(TAG, "detected GT911 touch controller on address 0x%02x", ESP_LCD_TOUCH_IO_I2C_GT911_ADDRESS_BACKUP);
    } else if (rust_i2c_probe(ESP_LCD_TOUCH_IO_I2C_TT21100_ADDRESS) == ESP_OK) {
        cfg_io_lt = cfg_lpiic_tt21100();
        cfg_lt.flags.mirror_x = true;
        touch_type = TOUCH_TT21100;
        ESP_LOGI(TAG, "detected TT21100 touch controller on address 0x%02x", ESP_LCD_TOUCH_IO_I2C_TT21100_ADDRESS);
    } else {
        ESP_LOGE(TAG, "touch screen not detected");
        return ESP_ERR_NOT_FOUND;
    }

    cfg_io_lt.scl_speed_hz = 400000;

    ret = esp_lcd_new_panel_io_i2c_v2(rust_i2c_master_handle(), &cfg_io_lt, &hdl_touch_io);
    if (ret != ESP_OK) {
        ESP_LOGE(TAG, "failed to initialize display panel IO: %s", esp_err_to_name(ret));
        return ret;
    }

    esp_lcd_touch_handle_t hdl_lt = NULL;

    if (touch_type == TOUCH_GT911) {
        ret = esp_lcd_touch_new_i2c_gt911(hdl_touch_io, &cfg_lt, &hdl_lt);
        if (ret != ESP_OK) {
            ESP_LOGE(TAG, "failed to initialize GT911 touch screen: %s", esp_err_to_name(ret));
            return ret;
        }
    } else if (touch_type == TOUCH_TT21100) {
        ret = esp_lcd_touch_new_i2c_tt21100(hdl_touch_io, &cfg_lt, &hdl_lt);
        if (ret != ESP_OK) {
            ESP_LOGE(TAG, "failed to initialize TT21100 touch screen: %s", esp_err_to_name(ret));
            return ret;
        }
    }
    const lvgl_port_touch_cfg_t cfg_pt = {
        .disp = ld,
        .handle = hdl_lt,
    };

    lv_indev_t *lt = lvgl_port_add_touch(&cfg_pt);
    lv_indev_enable(lt, true);

    LV_IMG_DECLARE(lv_img_hand_left);
    lv_obj_t *oc = lv_img_create(lv_scr_act());
    lv_img_set_src(oc, &lv_img_hand_left);
    lv_indev_set_cursor(lt, oc);

    return ret;
}
