#include "esp_event.h"
extern char was_url[2048];

void willow_was_event_handler(
    void *arg, esp_event_base_t event_base, int32_t event_id, void *event_data);
