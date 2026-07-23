#pragma once

#include "freertos/queue.h"

enum {
    WILLOW_QUEUE_TYPE_MUTEX = queueQUEUE_TYPE_MUTEX,
    WAS_RECONNECT_TIMEOUT_MS = 10 * 1000,
};
