enum willow_hw_t {
    WILLOW_HW_UNSUPPORTED = 0,
    WILLOW_HW_ESP32_S3_BOX,
    WILLOW_HW_ESP32_S3_BOX_LITE,
    WILLOW_HW_ESP32_S3_BOX_3,
    WILLOW_HW_MAX, // keep this last
};

extern enum willow_hw_t hw_type;

const char *str_hw_type(int id);
void init_system(void);
void restart_delayed(void);
