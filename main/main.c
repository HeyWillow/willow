#include "esp_err.h"
#include "esp_log.h"
#include "esp_lvgl_port.h"
#include "esp_netif.h"
#include "esp_ota_ops.h"
#include "esp_timer.h"
#include "lvgl.h"
#include "nvs.h"
#include "periph_spiffs.h"
#include "sdkconfig.h"

#include "audio.h"
#include "config.h"
#include "display.h"
#include "input.h"
#include "log.h"
#include "main.h"
#include "rust.h"
#include "shared.h"
#include "slvgl.h"
#include "system.h"
#include "tasks.h"
#include "timer.h"
#include "ui.h"
#include "was.h"

#ifdef CONFIG_MBEDTLS_SSL_PROTO_TLS1_3
#include "psa/crypto.h"
#endif

#if defined(CONFIG_WILLOW_ETHERNET)
#include "net/ethernet.h"
#endif

#define DEFAULT_WIS_URL "https://infer.tovera.io/api/willow"

#define I2S_PORT       I2S_NUM_0
#define PARTLABEL_USER "user"

char was_url[2048];
static const char *TAG = "WILLOW/MAIN";

esp_periph_set_handle_t hdl_pset;

static esp_err_t init_spiffs_user(void)
{
    esp_err_t ret = ESP_OK;
    periph_spiffs_cfg_t pcfg_spiffs_user = {
        .format_if_mount_failed = false,
        .max_files = 5,
        .partition_label = PARTLABEL_USER,
        .root = "/spiffs/user",
    };
    esp_periph_handle_t phdl_spiffs_user = periph_spiffs_init(&pcfg_spiffs_user);
    ret = esp_periph_start(hdl_pset, phdl_spiffs_user);
    if (ret != ESP_OK) {
        ESP_LOGE(TAG, "failed to start spiffs user peripheral: %s", esp_err_to_name(ret));
        return ret;
    }

    while (!periph_spiffs_is_mounted(phdl_spiffs_user)) {
        ESP_LOGI(TAG, "Waiting on SPIFFS mount...");
        vTaskDelay(500 / portTICK_PERIOD_MS);
    }
    ESP_LOGI(TAG, "SPIFFS mounted");

    return ret;
}

void willow_init(void)
{
    esp_err_t err;

    init_logging();
    ESP_LOGI(TAG, "Starting up! Please wait...");

    esp_periph_config_t pcfg = DEFAULT_ESP_PERIPH_SET_CONFIG();
    hdl_pset = esp_periph_set_init(&pcfg);

    init_system();
    init_spiffs_user();
    config_parse();
    init_display();
    init_lvgl_display();
    init_ui();

    ESP_ERROR_CHECK(esp_netif_init());

#ifdef CONFIG_WILLOW_ETHERNET
    init_ethernet();
#else
    nvs_handle_t hdl_nvs;
    err = nvs_open("WIFI", NVS_READONLY, &hdl_nvs);
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "failed to open NVS namespace WIFI: %s", esp_err_to_name(err));
        goto err_nvs;
    }

    char psk[64];
    size_t sz = sizeof(psk);
    err = nvs_get_str(hdl_nvs, "PSK", psk, &sz);
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "failed to get PSK from NVS namespace WIFI: %s", esp_err_to_name(err));
        goto err_nvs;
    }

    char ssid[33];
    sz = sizeof(ssid);
    err = nvs_get_str(hdl_nvs, "SSID", ssid, &sz);
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "failed to get PSK from NVS namespace WIFI: %s", esp_err_to_name(err));
        goto err_nvs;
    }
    if (lvgl_port_lock(lvgl_lock_timeout)) {
        lv_obj_clear_flag(lbl_ln4, LV_OBJ_FLAG_HIDDEN);
        lv_obj_set_style_text_align(lbl_ln4, LV_TEXT_ALIGN_CENTER, 0);
        lv_label_set_text_static(lbl_ln4, "Connecting to Wi-Fi...");
        lvgl_port_unlock();
    }

    (void)rust_wifi_init(psk, ssid, &hdl_netif);
#endif

    err = nvs_open("WAS", NVS_READONLY, &hdl_nvs);
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "failed to open NVS namespace WAS: %s", esp_err_to_name(err));
        goto err_nvs;
    }

#ifdef CONFIG_MBEDTLS_SSL_PROTO_TLS1_3
    // initialize mbedtls PSA library after wifi to have entropy
    psa_status_t status = psa_crypto_init();
    if (status != PSA_SUCCESS) {
        ESP_LOGE(TAG, "failed to initialize Mbed TLS PSA library, TLS will not work");
    }
#endif

    sz = sizeof(was_url);
    err = nvs_get_str(hdl_nvs, "URL", was_url, &sz);
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "failed to get WASL URL from NVS namespace WAS: %s", esp_err_to_name(err));
        goto err_nvs;
    }
    rust_state_mark_nvs_ok();
    err = init_was();
    if (err != ESP_OK) {
        ESP_LOGE(TAG, "failed to initialize Willow Application Server connection");
        ui_pr_err("Fatal error!", "WAS initialization failed.");
    }

    if (!config_valid) {
        // wait "indefinitely"
        vTaskDelay(portMAX_DELAY);
    }

// we jump over WAS initialization was without Wi-Fi this will never work
err_nvs:
    if (!rust_state_is_nvs_ok()) {
        ui_pr_err("Fatal error!", "Failed to read NVS partition.");
        // wait "indefinitely"
        vTaskDelay(portMAX_DELAY);
    }

    init_buttons();
    init_input_key_service();
    init_audio();
    init_lvgl_touch();
    init_display_timer();

#ifndef CONFIG_WILLOW_ETHERNET
    rust_get_mac_address(); // should be on wifi by now; print the MAC
#endif

    const esp_app_desc_t *app_desc = esp_app_get_description();
    ESP_LOGI(TAG, "Startup complete! Hardware: %s. Version: %s. Waiting for wake word.", str_hw_type(hw_type),
             app_desc->version);

    // if we reached this point, we can mark the current partition valid
    // we can still crash on wake or other events but we should be able to do another OTA
    // we can also still crash in the while loop below - this should be improved
    ESP_ERROR_CHECK_WITHOUT_ABORT(esp_ota_mark_app_valid_cancel_rollback());

    ESP_ERROR_CHECK_WITHOUT_ABORT(
        reset_timer(hdl_display_timer, config_get_int("display_timeout", DEFAULT_DISPLAY_TIMEOUT), false));

#ifdef CONFIG_WILLOW_DEBUG_RUNTIME_STATS
    xTaskCreate(&task_debug_runtime_stats, "dbg_runtime_stats", 4 * 1024, NULL, 0, NULL);
#endif
}

void willow_main_loop_iteration(void)
{
#ifdef CONFIG_WILLOW_DEBUG_MEM
    printf("MALLOC_CAP_INTERNAL:\n");
    heap_caps_print_heap_info(MALLOC_CAP_INTERNAL);
    printf("MALLOC_CAP_SPIRAM:\n");
    heap_caps_print_heap_info(MALLOC_CAP_SPIRAM);
#endif
#ifdef CONFIG_WILLOW_DEBUG_TASKS
    char buf[128];
    vTaskList(buf);
    printf("%s\n", buf);
#endif
#ifdef CONFIG_WILLOW_DEBUG_TIMERS
    (esp_timer_dump(stdout));
#endif
    vTaskDelay(5000 / portTICK_PERIOD_MS);
}

#ifdef WILLOW_CARGO_FIRST
// esp-idf-sys needs an app_main symbol for its intermediate C link. The final
// Cargo link replaces this weak stub with Rust's strong entry point.
void __attribute__((weak)) app_main(void) {}
#endif
