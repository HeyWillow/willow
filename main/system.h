#include "esp_peripherals.h"

enum willow_hw_t {
    WILLOW_HW_UNSUPPORTED = 0,
    WILLOW_HW_ESP32_S3_BOX,
    WILLOW_HW_ESP32_S3_BOX_LITE,
    WILLOW_HW_ESP32_S3_BOX_3,
    WILLOW_HW_MAX, // keep this last
};

enum willow_state {
    STATE_INIT = 0,
    STATE_NVS_OK,
    STATE_CONFIG_OK,
    STATE_READY,
    STATE_NEED_CONFIG,
    STATE_WRITE_FLASH,
    STATE_RESTARTING,
};

extern enum willow_hw_t hw_type;
extern volatile enum willow_state state;
extern esp_periph_set_handle_t hdl_pset;

const char *str_hw_type(int id);
void init_system(void);
void restart_delayed(void);
