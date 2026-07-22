#include "esp_timer.h"

extern esp_timer_handle_t hdl_sess_timer;

esp_err_t init_session_timer(void);
esp_err_t reset_timer(esp_timer_handle_t hdl, int timeout, bool pause);
