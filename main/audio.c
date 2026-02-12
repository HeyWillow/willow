#include "amrwb_encoder.h"
#include "audio_hal.h"
#include "audio_mem.h"
#include "audio_pipeline.h"
#include "audio_thread.h"
#include "board.h"
#include "esp_check.h"
#include "esp_decoder.h"
#include "esp_http_client.h"
#include "esp_log.h"
#include "esp_lvgl_port.h"
#include "esp_task_wdt.h"
#include "esp_timer.h"
#include "filter_resample.h"
#include "flac_decoder.h"
#include "http_stream.h"
#include "i2s_stream.h"
#include "lvgl.h"
#include "model_path.h"
#include "raw_stream.h"
#include "recorder_encoder.h"
#include "recorder_sr.h"
#include "sdkconfig.h"
#include "spiffs_stream.h"
#include "wav_decoder.h"
#include "wav_encoder.h"

#include "audio.h"
#include "config.h"
#include "display.h"
#include "shared.h"
#include "slvgl.h"
#include "timer.h"
#include "ui.h"
#include "was.h"

#include "endpoint/hass.h"
#include "endpoint/openhab.h"
#include "endpoint/rest.h"

#if !defined(CONFIG_TASK_WDT_PANIC)
#define CONFIG_TASK_WDT_PANIC 10
#endif

#if defined(WILLOW_SUPPORT_MULTINET)
#include "generated_cmd_multinet.h"
#endif

#define DEFAULT_AUDIO_CODEC         "PCM"
#define DEFAULT_AUDIO_RESPONSE_TYPE "None"
#define DEFAULT_RECORD_BUFFER       12
#define DEFAULT_SPEAKER_VOLUME      60
#define DEFAULT_SPEECH_REC_MODE     "WIS"
#define DEFAULT_STREAM_TIMEOUT      5
#define DEFAULT_VAD_MODE            3
#define DEFAULT_VAD_TIMEOUT         300
#define DEFAULT_WAKE_MODE           "2CH_90"
#define DEFAULT_WAKE_WORD           "hiesp"
#define DEFAULT_WIS_TTS_URL         "https://infer.tovera.io/api/tts"
#define DEFAULT_WIS_URL             "https://infer.tovera.io/api/willow"

#define HTTP_STREAM_TIMEOUT_MS              2 * 1000
#define HTTP_STREAM_TIMEOUT_MS_POST_REQUEST 10 * 1000

#define MULTINET_TWDT   30
#define STR_WAKE_LEN    25
#define WIS_URL_TTS_ARG "?format=WAV&speaker=CLB&text="

typedef enum willow_http_stream {
    WILLOW_HS_ESP_AUDIO,
    WILLOW_HS_STT,
} willow_http_stream_t;

QueueHandle_t q_ea, q_rec;
audio_hal_handle_t hdl_aha = NULL, hdl_ahc = NULL;
audio_rec_handle_t hdl_ar = NULL;
esp_audio_handle_t hdl_ea = NULL;
volatile bool multiwake_won = false;
volatile bool recording = false;
static audio_element_handle_t hdl_ae_hs, hdl_ae_rs_from_i2s, hdl_ae_rs_to_api = NULL;
static audio_pipeline_handle_t hdl_ap, hdl_ap_to_api;
static audio_thread_t hdl_at = NULL;
static bool stream_to_api = false;
static const char *TAG = "WILLOW/AUDIO";
static int total_write = 0;
struct willow_audio_response war;

static void cb_ea(esp_audio_state_t *state, void *data)
{
    ESP_LOGD(TAG, "ESP Audio Event received: %d", state->status);
    if (state->status > AUDIO_STATUS_RUNNING) {
        gpio_set_level(get_pa_enable_gpio(), 0);
    }
}

static void play_audio(const char *uri)
{
    reset_timer(hdl_display_timer, config_get_int("display_timeout", DEFAULT_DISPLAY_TIMEOUT), false);
    display_set_backlight(true, false);

    if (hdl_ea == NULL) {
        ESP_LOGE(TAG, "audio_play called with hdl_ea=NULL, skip audio playback");
        return;
    }

    audio_err_t err = esp_audio_play(hdl_ea, AUDIO_CODEC_TYPE_DECODER, uri, 0);

    if (err == ESP_ERR_AUDIO_OPEN) {
        ESP_LOGE(TAG, "ESP_ERR_AUDIO_OPEN from ESP Audio. Reinitialize audio.");
        deinit_audio();
        init_audio();
    }
}

static void check_mute(void)
{
    int gpio_level = gpio_get_level(GPIO_NUM_1);
    if (gpio_level == 0) {
        ESP_LOGW(TAG, "mute is activated, please unmute to continue startup");
        ui_pr_err("Mute Activated", "Unmute to continue");
        while (gpio_get_level(GPIO_NUM_1) == 0) {
            vTaskDelay(1000 / portTICK_PERIOD_MS);
        }
    }
}

static void play_audio_err(void *data)
{
    gpio_set_level(get_pa_enable_gpio(), 1);
    play_audio("spiffs://spiffs/user/audio/error.wav");
}

void play_audio_ok(void *data)
{
    gpio_set_level(get_pa_enable_gpio(), 1);
    play_audio("spiffs://spiffs/user/audio/success.wav");
}

static void play_audio_wis_tts(void *data)
{
    char *url = NULL;
    char *wis_tts_url = NULL;

    if (data == NULL) {
        ESP_LOGW(TAG, "called play_audio_wis_tts with NULL data");
        return;
    }

    wis_tts_url = config_get_char("wis_tts_url_v2", NULL);
    if (wis_tts_url != NULL) {
        int len_url = strlen(wis_tts_url) + strlen((char *)data) + 1;
        url = calloc(len_url, sizeof(char));
        snprintf(url, len_url, "%s%s", wis_tts_url, (char *)data);
    } else {
        wis_tts_url = config_get_char("wis_tts_url", DEFAULT_WIS_TTS_URL);
        int len_url = strlen(wis_tts_url) + strlen(WIS_URL_TTS_ARG) + strlen((char *)data) + 1;
        url = calloc(len_url, sizeof(char));
        snprintf(url, len_url, "%s%s%s", wis_tts_url, WIS_URL_TTS_ARG, (char *)data);
    }

    free(wis_tts_url);
    gpio_set_level(get_pa_enable_gpio(), 1);
    ESP_LOGI(TAG, "Using WIS TTS URL '%s'", url);
    play_audio(url);
    free(url);
}

static void noop(void *data)
{
}

static void init_audio_response(void)
{
    char *audio_response_type = config_get_char("audio_response_type", DEFAULT_AUDIO_RESPONSE_TYPE);
    if (strcmp(audio_response_type, "Chimes") == 0) {
        war.fn_err = play_audio_err;
        war.fn_ok = play_audio_ok;
    } else if (strcmp(audio_response_type, "TTS") == 0) {
        war.fn_err = play_audio_wis_tts;
        war.fn_ok = play_audio_wis_tts;
    } else {
        war.fn_err = noop;
        war.fn_ok = noop;
    }
    free(audio_response_type);
}

static esp_err_t cb_ae_hs(audio_element_handle_t el, audio_event_iface_msg_t *ev, void *data)
{
    if (ev->cmd == AEL_MSG_CMD_REPORT_STATUS) {
        // if we get a AEL_MSG_CMD_REPORT_STATUS command we can check for errors in audio_element_status_t
        // 1-7 is error
        int ae_status = (int)ev->data;
        ESP_LOGI(TAG, "event_cb_func(): AEL_MSG_CMD_REPORT_STATUS: %d", ae_status);

        if (ae_status == 0 || ae_status > 7) {
            return ESP_OK;
        }

        willow_http_stream_t type_hs = (willow_http_stream_t)data;
        if (type_hs == WILLOW_HS_STT) {
            audio_recorder_trigger_stop(hdl_ar);
            war.fn_err("Cannot Reach WIS");
            ESP_LOGE(TAG, "Error opening STT endpoint (%d)", ae_status);
            ui_pr_err("Cannot Reach WIS", "Check Server & Settings");
        } else if (type_hs == WILLOW_HS_ESP_AUDIO) {
            play_audio_err(NULL);
            ESP_LOGE(TAG, "Error opening ESP Audio endpoint (%d)", ae_status);
            ui_pr_err("Cannot Reach WIS", "Check Server & Settings");
        }
    }
    return ESP_OK;
}

static esp_err_t hdl_ev_hs_esp_audio(http_stream_event_msg_t *msg)
{
    if (msg == NULL) {
        return ESP_ERR_INVALID_ARG;
    }

    esp_http_client_handle_t http = (esp_http_client_handle_t)msg->http_client;

    switch (msg->event_id) {
        case HTTP_STREAM_PRE_REQUEST:
            esp_http_client_set_authtype(http, HTTP_AUTH_TYPE_BASIC);
            esp_http_client_set_timeout_ms(http, HTTP_STREAM_TIMEOUT_MS);
            break;
        case HTTP_STREAM_POST_REQUEST:
            esp_http_client_set_timeout_ms(http, HTTP_STREAM_TIMEOUT_MS_POST_REQUEST);
            break;
        default:
            break;
    }
    return ESP_OK;
}

static void init_esp_audio(void)
{
    audio_err_t ret = ESP_OK;
    q_ea = xQueueCreate(3, sizeof(esp_audio_state_t));
    esp_audio_cfg_t cfg_ea = {
        .cb_ctx = NULL,
        .cb_func = cb_ea,
        .component_select = ESP_AUDIO_COMPONENT_SELECT_DEFAULT,
        .evt_que = q_ea,
        .in_stream_buf_size = 10 * 1024,
        .out_stream_buf_size = 4 * 1024,
        .prefer_type = ESP_AUDIO_PREFER_SPEED,
        .resample_rate = 16000,
        .task_prio = 6,
        .task_stack = 4 * 1024,
        .vol_get = (audio_volume_get)audio_hal_get_volume,
        .vol_handle = hdl_ahc,
        .vol_set = (audio_volume_set)audio_hal_set_volume,
    };

    hdl_ea = esp_audio_create(&cfg_ea);

    http_stream_cfg_t cfg_hs = HTTP_STREAM_CFG_DEFAULT();
    cfg_hs.event_handle = hdl_ev_hs_esp_audio;

    audio_element_handle_t hdl_ae_hs = http_stream_init(&cfg_hs);
    audio_element_set_event_callback(hdl_ea, cb_ae_hs, (void *)WILLOW_HS_ESP_AUDIO);

    ret = esp_audio_input_stream_add(hdl_ea, hdl_ae_hs);
    if (ret != ESP_OK) {
        ESP_LOGE(TAG, "failed to add HTTP input stream to ESP Audio");
    }

    spiffs_stream_cfg_t cfg_ss = SPIFFS_STREAM_CFG_DEFAULT();
    cfg_ss.type = AUDIO_STREAM_READER;

    ret = esp_audio_input_stream_add(hdl_ea, spiffs_stream_init(&cfg_ss));
    if (ret != ESP_OK) {
        ESP_LOGE(TAG, "failed to add input stream to ESP Audio");
    }

    // disable clang-format to allow each member on a new line
    // clang-format off
    audio_decoder_t ad[] = {
        DEFAULT_ESP_AAC_DECODER_CONFIG(),
        DEFAULT_ESP_AMRNB_DECODER_CONFIG(),
        DEFAULT_ESP_AMRWB_DECODER_CONFIG(),
        DEFAULT_ESP_FLAC_DECODER_CONFIG(),
        DEFAULT_ESP_M4A_DECODER_CONFIG(),
        DEFAULT_ESP_MP3_DECODER_CONFIG(),
        DEFAULT_ESP_OGG_DECODER_CONFIG(),
        DEFAULT_ESP_OPUS_DECODER_CONFIG(),
        DEFAULT_ESP_PCM_DECODER_CONFIG(),
        DEFAULT_ESP_TS_DECODER_CONFIG(),
        DEFAULT_ESP_WAV_DECODER_CONFIG(),
    };
    // clang-format on

    esp_decoder_cfg_t cfg_dec = {
        .out_rb_size = ESP_DECODER_RINGBUFFER_SIZE,
        .plus_enable = true,
        .stack_in_ext = true,
        .task_core = 0,
        .task_prio = ESP_DECODER_TASK_PRIO,
        .task_stack = ESP_DECODER_TASK_STACK_SIZE,
    };

    ret = esp_audio_codec_lib_add(hdl_ea, AUDIO_CODEC_TYPE_DECODER,
                                  esp_decoder_init(&cfg_dec, ad, sizeof(ad) / sizeof(audio_decoder_t)));
    if (ret != ESP_OK) {
        ESP_LOGE(TAG, "failed to add decoder to ESP Audio");
    }

    i2s_stream_cfg_t cfg_is = {
        .buffer_len = I2S_STREAM_BUF_SIZE,
        .chan_cfg = {
            .auto_clear = true,
            .dma_desc_num = 3,
            .dma_frame_num = 312,
            .id = CODEC_ADC_I2S_PORT,
            .role = I2S_ROLE_MASTER,
        },
        .expand_src_bits = I2S_DATA_BIT_WIDTH_16BIT,
        .multi_out_num = 0,
        .need_expand = true,
        .out_rb_size = 8 * 1024, // default is 8 * 1024
        .stack_in_ext = false,
        .std_cfg = {
            .clk_cfg  = {
                .clk_src = I2S_CLK_SRC_DEFAULT,
                .mclk_multiple = I2S_MCLK_MULTIPLE_256,
                .sample_rate_hz = 16000,
            },
            .gpio_cfg = {
                .invert_flags = {
                    .bclk_inv = false,
                    .mclk_inv = false,
                },
            },
            .slot_cfg = {
                .big_endian = false,
                .bit_order_lsb = false,
                .bit_shift = true,
                .data_bit_width = I2S_DATA_BIT_WIDTH_32BIT,
                .left_align = true,
                .slot_bit_width = I2S_SLOT_BIT_WIDTH_AUTO,
                .slot_mask = I2S_STD_SLOT_BOTH,
                .slot_mode = I2S_SLOT_MODE_STEREO,
                .ws_pol = false,
                .ws_width = I2S_DATA_BIT_WIDTH_32BIT,
            }
        },
        .task_core = I2S_STREAM_TASK_CORE,
        .task_prio = I2S_STREAM_TASK_PRIO,
        .task_stack = I2S_STREAM_TASK_STACK,
        .type = AUDIO_STREAM_WRITER,
        .uninstall_drv = true,
        .use_alc = false,
        .volume = 0,
    };

    ret = esp_audio_output_stream_add(hdl_ea, i2s_stream_init(&cfg_is));
    if (ret != ESP_OK) {
        ESP_LOGE(TAG, "failed to add output stream to ESP Audio");
    }
    esp_audio_vol_set(hdl_ea, config_get_int("speaker_volume", DEFAULT_SPEAKER_VOLUME));
    ESP_LOGI(TAG, "audio player initialized");
}

static esp_err_t cb_ar_event(audio_rec_evt_t *are, void *data)
{
    char *speech_rec_mode = NULL;
    int msg = -1;
#if defined(WILLOW_SUPPORT_MULTINET)
    int command_id = 0;
#endif

    switch (are->type) {
        case AUDIO_REC_VAD_END:
            ESP_LOGI(TAG, "AUDIO_REC_VAD_END");
            if (esp_timer_is_active(hdl_sess_timer)) {
                esp_timer_stop(hdl_sess_timer);
            }
            break;
        case AUDIO_REC_VAD_START:
            ESP_LOGI(TAG, "AUDIO_REC_VAD_START");
            if (recording) {
                break;
            } else {
                recording = true;
            }

            speech_rec_mode = config_get_char("speech_rec_mode", DEFAULT_SPEECH_REC_MODE);
            if (strcmp(speech_rec_mode, "Multinet") == 0) {
                msg = MSG_START_LOCAL;
            } else if (strcmp(speech_rec_mode, "WIS") == 0) {
                msg = MSG_START;
            } else {
                free(speech_rec_mode);
                return ESP_ERR_INVALID_ARG;
            }
            free(speech_rec_mode);
            xQueueSend(q_rec, &msg, 0);
            break;
        case AUDIO_REC_COMMAND_DECT:
            // Multinet timeout
            ESP_LOGI(TAG, "AUDIO_REC_COMMAND_DECT");
            war.fn_err("unrecognized command");
            if (lvgl_port_lock(lvgl_lock_timeout)) {
                lv_obj_clear_flag(lbl_ln4, LV_OBJ_FLAG_HIDDEN);
                lv_obj_set_style_text_align(lbl_ln4, LV_TEXT_ALIGN_LEFT, 0);
                lv_label_set_text(lbl_ln4, "#ff0000 Unrecognized Command");
                lvgl_port_unlock();
            }

            reset_timer(hdl_display_timer, config_get_int("display_timeout", DEFAULT_DISPLAY_TIMEOUT), false);
            break;
        case AUDIO_REC_WAKEUP_END:
            ESP_LOGI(TAG, "AUDIO_REC_WAKEUP_END");
            msg = MSG_STOP;
            xQueueSend(q_rec, &msg, 0);
            send_wake_end();
            break;
        case AUDIO_REC_WAKEUP_START:
            ESP_LOGI(TAG, "AUDIO_REC_WAKEUP_START");
            if (recording) {
                break;
            }
            if (!config_get_bool("multiwake", false)) {
                if (config_get_bool("wake_confirmation", DEFAULT_WAKE_CONFIRMATION)) {
                    play_audio_ok(NULL);
                }
            }
            // win by default so in case WAS multiwake handling goes wrong we act normally
            multiwake_won = true;
            recorder_sr_wakeup_result_t *wake_data = are->event_data;
            ESP_LOGI(TAG, "wake volume: %f", wake_data->data_volume);
            send_wake_start(wake_data->data_volume);
            reset_timer(hdl_display_timer, config_get_int("display_timeout", DEFAULT_DISPLAY_TIMEOUT), true);

            speech_rec_mode = config_get_char("speech_rec_mode", DEFAULT_SPEECH_REC_MODE);

            if (strcmp(speech_rec_mode, "WIS") == 0) {
                reset_timer(hdl_sess_timer, config_get_int("stream_timeout", DEFAULT_STREAM_TIMEOUT), false);
            }
            if (lvgl_port_lock(lvgl_lock_timeout)) {
                lv_obj_add_flag(lbl_ln1, LV_OBJ_FLAG_HIDDEN);
                lv_obj_add_flag(lbl_ln2, LV_OBJ_FLAG_HIDDEN);
                lv_obj_add_flag(lbl_ln4, LV_OBJ_FLAG_HIDDEN);
                lv_obj_add_flag(lbl_ln5, LV_OBJ_FLAG_HIDDEN);
                lv_obj_clear_flag(btn_cancel, LV_OBJ_FLAG_HIDDEN);
                lv_obj_clear_flag(lbl_ln3, LV_OBJ_FLAG_HIDDEN);

                if (strcmp(speech_rec_mode, "Multinet") == 0) {
                    lv_label_set_text_static(lbl_ln3, "Say local command...");
                } else if (strcmp(speech_rec_mode, "WIS") == 0) {
                    lv_label_set_text_static(lbl_ln3, "Say command...");
                } else {
                    return ESP_ERR_INVALID_ARG;
                }

                lv_obj_add_event_cb(btn_cancel, cb_btn_cancel, LV_EVENT_PRESSED, NULL);
                lvgl_port_unlock();
            }
            free(speech_rec_mode);
            display_set_backlight(true, false);
            break;
        default:
            speech_rec_mode = config_get_char("speech_rec_mode", DEFAULT_SPEECH_REC_MODE);
            if (strcmp(speech_rec_mode, "Multinet") == 0) {
#if defined(WILLOW_SUPPORT_MULTINET)
                // Catch all for local commands
                command_id = are->type;
                bool was_mode = config_get_bool("was_mode", DEFAULT_WAS_MODE);
                char *command_endpoint = config_get_char("command_endpoint", DEFAULT_COMMAND_ENDPOINT);
                char *json;
                json = calloc(29 + strlen(lookup_cmd_multinet(command_id)), sizeof(char));
                snprintf(json, 29 + strlen(lookup_cmd_multinet(command_id)), "{\"text\":\"%s\",\"language\":\"en\"}",
                         lookup_cmd_multinet(command_id));
                if (was_mode) {
                    was_send_endpoint(json, false);
                } else if (strcmp(command_endpoint, "Home Assistant") == 0) {
                    hass_send(json);
                } else if (strcmp(command_endpoint, "openHAB") == 0) {
                    openhab_send(lookup_cmd_multinet(command_id));
                } else if (strcmp(command_endpoint, "REST") == 0) {
                    rest_send(json);
                }
                free(command_endpoint);
                free(json);

                ESP_LOGI(TAG, "Got local command ID: '%d'", command_id);
                if (lvgl_port_lock(lvgl_lock_timeout)) {
                    lv_obj_clear_flag(lbl_ln1, LV_OBJ_FLAG_HIDDEN);
                    lv_obj_clear_flag(lbl_ln2, LV_OBJ_FLAG_HIDDEN);
                    lv_obj_add_flag(lbl_ln3, LV_OBJ_FLAG_HIDDEN);

                    lv_label_set_text_static(lbl_ln1, "I heard command:");
                    lv_label_set_text(lbl_ln2, lookup_cmd_multinet(command_id));
                    lvgl_port_unlock();
                }
                reset_timer(hdl_display_timer, config_get_int("display_timeout", DEFAULT_DISPLAY_TIMEOUT), false);
#else
                ESP_LOGE(TAG, "multinet not supported but enabled in config");
#endif
            } else {
                ESP_LOGI(TAG, "cb_ar_event: unhandled event: '%d'", are->type);
            }
            free(speech_rec_mode);
            break;
    }

    return ESP_OK;
}

static int feed_afe(int16_t *buf, int len, void *ctx, TickType_t ticks)
{
    if (buf == NULL || hdl_ae_rs_from_i2s == NULL) {
        return -1;
    }

    return raw_stream_read(hdl_ae_rs_from_i2s, (char *)buf, len);
}

static esp_err_t hdl_ev_hs_to_api(http_stream_event_msg_t *msg)
{
    if (msg == NULL) {
        return ESP_ERR_INVALID_ARG;
    }

    esp_http_client_handle_t http = (esp_http_client_handle_t)msg->http_client;
    char len_buf[16];
    int wlen = 0;

    switch (msg->event_id) {
        case HTTP_STREAM_PRE_REQUEST:
            ESP_LOGI(TAG, "WIS HTTP client starting stream, waiting for end of speech");
            esp_http_client_set_authtype(http, HTTP_AUTH_TYPE_BASIC);
            esp_http_client_set_method(http, HTTP_METHOD_POST);
            esp_http_client_set_timeout_ms(http, HTTP_STREAM_TIMEOUT_MS);
            char *audio_codec = config_get_char("audio_codec", DEFAULT_AUDIO_CODEC);
            char dat[10] = {0};
            snprintf(dat, sizeof(dat), "%d", 16000);
            esp_http_client_set_header(http, "x-audio-sample-rate", dat);
            memset(dat, 0, sizeof(dat));
            snprintf(dat, sizeof(dat), "%d", 16);
            esp_http_client_set_header(http, "x-audio-bits", dat);
            memset(dat, 0, sizeof(dat));
            snprintf(dat, sizeof(dat), "%d", 1);
            esp_http_client_set_header(http, "x-audio-channel", dat);
            if (strcmp(audio_codec, "AMR-WB") == 0) {
                esp_http_client_set_header(http, "x-audio-codec", "amrwb");
            } else if (strcmp(audio_codec, "WAV") == 0) {
                esp_http_client_set_header(http, "x-audio-codec", "wav");
            } else if (strcmp(audio_codec, "PCM") == 0) {
                esp_http_client_set_header(http, "x-audio-codec", "pcm");
            }
            free(audio_codec);
            total_write = 0;
            return ESP_OK;

        case HTTP_STREAM_ON_REQUEST:
            wlen = sprintf(len_buf, "%x\r\n", msg->buffer_len);
            if (esp_http_client_write(http, len_buf, wlen) <= 0) {
                return ESP_FAIL;
            }
            if (esp_http_client_write(http, msg->buffer, msg->buffer_len) <= 0) {
                return ESP_FAIL;
            }
            if (esp_http_client_write(http, "\r\n", 2) <= 0) {
                return ESP_FAIL;
            }
            total_write += msg->buffer_len;
            // ESP_LOGI(TAG, "WIS HTTP client total bytes written: %d", total_write);
            return msg->buffer_len;

        case HTTP_STREAM_POST_REQUEST:
            ESP_LOGI(TAG, "WIS HTTP client HTTP_STREAM_POST_REQUEST, write end chunked marker");
            esp_http_client_set_timeout_ms(http, HTTP_STREAM_TIMEOUT_MS_POST_REQUEST);
            if (esp_http_client_write(http, "0\r\n\r\n", 5) <= 0) {
                return ESP_FAIL;
            }
            return ESP_OK;

        case HTTP_STREAM_FINISH_REQUEST:
            ESP_LOGI(TAG, "WIS HTTP client HTTP_STREAM_FINISH_REQUEST");
            // bail out if we didn't win the multiwake race
            if (!multiwake_won) {
                goto pause;
            }
            // Check status code
            int http_status = esp_http_client_get_status_code(http);
            if (http_status != 200) {
                // when ESP HTTP Client terminates connection due to timeout we get -1
                if (http_status == -1) {
                    ESP_LOGE(TAG, "WIS response took longer than %dms, connection aborted", HTTP_STREAM_TIMEOUT_MS);
                    ui_pr_err("WIS timeout", "Check server performance");
                } else if (http_status == 401) {
                    ESP_LOGE(TAG, "WIS returned Unauthorized Access (HTTP 401)");
                    ui_pr_err("WIS auth failed", "Check server & settings");
                } else if (http_status == 406) {
                    ESP_LOGE(TAG, "WIS returned Unauthorized Speaker");
                    ui_pr_err("Unauthorized Speaker", NULL);
                    war.fn_err("Unauthorized Speaker");
                } else {
                    ESP_LOGE(TAG, "WIS returned HTTP error: %d", http_status);
                    char str_http_err[14];
                    snprintf(str_http_err, 14, "WIS HTTP %d", http_status);
                    ui_pr_err(str_http_err, NULL);
                    war.fn_err(str_http_err);
                }
                return ESP_FAIL;
            }
            // Allocate memory for response. Should be enough?
            char *buf = calloc(2048, sizeof(char));
            assert(buf);
            int read_len = esp_http_client_read(http, buf, 2048);
            if (read_len <= 0) {
                free(buf);
                return ESP_FAIL;
            }
            buf[read_len] = 0;
            ESP_LOGI(TAG, "WIS HTTP Response = %s", (char *)buf);
            if (lvgl_port_lock(lvgl_lock_timeout)) {
                lv_obj_add_flag(lbl_ln3, LV_OBJ_FLAG_HIDDEN);
                lv_obj_add_flag(lbl_ln4, LV_OBJ_FLAG_HIDDEN);
                lvgl_port_unlock();
            }
            bool was_mode = config_get_bool("was_mode", DEFAULT_WAS_MODE);
            char *command_endpoint = config_get_char("command_endpoint", DEFAULT_COMMAND_ENDPOINT);
            if (was_mode) {
                was_send_endpoint(buf, false);
            } else if (strcmp(command_endpoint, "Home Assistant") == 0) {
                hass_send(buf);
            } else if (strcmp(command_endpoint, "openHAB") == 0) {
                openhab_send(buf);
            } else if (strcmp(command_endpoint, "REST") == 0) {
                rest_send(buf);
            }
            free(command_endpoint);

            cJSON *cjson = cJSON_Parse(buf);
            cJSON *text = cJSON_GetObjectItemCaseSensitive(cjson, "text");
            cJSON *speaker_status = cJSON_GetObjectItemCaseSensitive(cjson, "speaker_status");

            if (lvgl_port_lock(lvgl_lock_timeout)) {
                lv_obj_clear_flag(lbl_ln1, LV_OBJ_FLAG_HIDDEN);
                lv_obj_clear_flag(lbl_ln2, LV_OBJ_FLAG_HIDDEN);
                if (cJSON_IsString(speaker_status) && speaker_status->valuestring != NULL) {
                    lv_label_set_text(lbl_ln1, speaker_status->valuestring);
                } else {
                    lv_label_set_text(lbl_ln1, "I heard:");
                }
                if (cJSON_IsString(text) && text->valuestring != NULL) {
                    lv_label_set_text(lbl_ln2, text->valuestring);
                } else {
                    lv_label_set_text(lbl_ln2, buf);
                }
                lvgl_port_unlock();
            }

            cJSON_Delete(cjson);
            free(buf);

pause:
            audio_pipeline_pause(hdl_ap_to_api);

            return ESP_OK;

        default:
            return ESP_ERR_INVALID_ARG;
    }
    return ESP_OK;
}

static esp_err_t init_ap_to_api(void)
{
    ESP_LOGD(TAG, "init_ap_to_api()");
    // audio_element_handle_t hdl_ae_hs;
    audio_pipeline_cfg_t cfg_ap = DEFAULT_AUDIO_PIPELINE_CONFIG();
    hdl_ap_to_api = audio_pipeline_init(&cfg_ap);

    http_stream_cfg_t cfg_hs = HTTP_STREAM_CFG_DEFAULT();
    cfg_hs.event_handle = hdl_ev_hs_to_api;
    cfg_hs.task_stack = 8 * 1024; // default 6 * 1024
    cfg_hs.type = AUDIO_STREAM_WRITER;
    cfg_hs.user_agent = WILLOW_USER_AGENT;
    hdl_ae_hs = http_stream_init(&cfg_hs);

    audio_element_set_event_callback(hdl_ae_hs, cb_ae_hs, (void *)WILLOW_HS_STT);

    raw_stream_cfg_t cfg_rs = RAW_STREAM_CFG_DEFAULT();
    cfg_rs.out_rb_size = 64 * 1024; // default is 8 * 1024
    cfg_rs.type = AUDIO_STREAM_WRITER;
    hdl_ae_rs_to_api = raw_stream_init(&cfg_rs);

    audio_pipeline_register(hdl_ap_to_api, hdl_ae_hs, "http_stream_writer");
    audio_pipeline_register(hdl_ap_to_api, hdl_ae_rs_to_api, "raw_stream_writer_to_api");

    char *wis_url = config_get_char("wis_url", DEFAULT_WIS_URL);
    const char *tag_link[2] = {"raw_stream_writer_to_api", "http_stream_writer"};
    audio_pipeline_link(hdl_ap_to_api, &tag_link[0], 2);
    audio_element_set_uri(hdl_ae_hs, wis_url);
    free(wis_url);

    audio_element_info_t info = AUDIO_ELEMENT_INFO_DEFAULT();
    audio_element_getinfo(hdl_ae_hs, &info);
    ESP_LOGI(TAG, "audio_element_getinfo(hdl_ae_hs): sample_rate='%d' channels='%d' bits='%d' bps = '%d'",
             info.sample_rates, info.channels, info.bits, info.bps);

    return ESP_OK;
}

static esp_err_t start_rec(void)
{
    audio_element_handle_t hdl_ae_is;
    audio_pipeline_cfg_t cfg_ap = DEFAULT_AUDIO_PIPELINE_CONFIG();
    esp_err_t ret = ESP_OK;

    hdl_ap = audio_pipeline_init(&cfg_ap);
    if (hdl_ap == NULL) {
        return ESP_FAIL;
    }

    i2s_stream_cfg_t cfg_is = {
        .buffer_len = I2S_STREAM_BUF_SIZE,
        .chan_cfg = {
            .auto_clear = true,
            .dma_desc_num = 3,
            .dma_frame_num = 312,
            .id = CODEC_ADC_I2S_PORT,
            .role = I2S_ROLE_MASTER,
        },
        .expand_src_bits = I2S_DATA_BIT_WIDTH_16BIT,
        .multi_out_num = 0,
        .need_expand = false,
        .out_rb_size = 8 * 1024, // default is 8 * 1024
        .stack_in_ext = false,
        .std_cfg = {
            .clk_cfg  = {
                .clk_src = I2S_CLK_SRC_DEFAULT,
                .mclk_multiple = I2S_MCLK_MULTIPLE_256,
                .sample_rate_hz = 44100,
            },
            .gpio_cfg = {
                .invert_flags = {
                    .bclk_inv = false,
                    .mclk_inv = false,
                },
            },
            .slot_cfg = {
                .big_endian = false,
                .bit_order_lsb = false,
                .bit_shift = true,
                .data_bit_width = I2S_DATA_BIT_WIDTH_16BIT,
                .left_align = true,
                .slot_bit_width = I2S_SLOT_BIT_WIDTH_AUTO,
                .slot_mask = I2S_STD_SLOT_BOTH,
                .slot_mode = I2S_SLOT_MODE_STEREO,
                .ws_pol = false,
                .ws_width = I2S_DATA_BIT_WIDTH_16BIT,
            },
        },
        .task_core = I2S_STREAM_TASK_CORE,
        .task_prio = I2S_STREAM_TASK_PRIO,
        .task_stack = I2S_STREAM_TASK_STACK,
        .type = AUDIO_STREAM_READER,
        .uninstall_drv = true,
        .use_alc = false,
        .volume = 0,
    };
    hdl_ae_is = i2s_stream_init(&cfg_is);

    i2s_stream_set_clk(hdl_ae_is, 16000, 32, 2);

    raw_stream_cfg_t cfg_rs = RAW_STREAM_CFG_DEFAULT();
    cfg_rs.type = AUDIO_STREAM_READER;
    hdl_ae_rs_from_i2s = raw_stream_init(&cfg_rs);

    audio_pipeline_register(hdl_ap, hdl_ae_is, "i2s_stream_reader");

    audio_pipeline_register(hdl_ap, hdl_ae_rs_from_i2s, "raw_stream_reader");

    const char *tag_link[2] = {"i2s_stream_reader", "raw_stream_reader"};
    audio_pipeline_link(hdl_ap, &tag_link[0], 2);

    audio_pipeline_run(hdl_ap);

    char *wake_mode = config_get_char("wake_mode", DEFAULT_WAKE_MODE);
    int wakenet_mode = -1;
    if (strcmp(wake_mode, "2CH_90") == 0) {
        wakenet_mode = DET_MODE_2CH_90;
    } else if (strcmp(wake_mode, "2CH_95") == 0) {
        wakenet_mode = DET_MODE_2CH_95;
    } else if (strcmp(wake_mode, "1CH_90") == 0) {
        wakenet_mode = DET_MODE_90;
    } else if (strcmp(wake_mode, "1CH_95") == 0) {
        wakenet_mode = DET_MODE_95;
    } else if (strcmp(wake_mode, "3CH_90") == 0) {
        wakenet_mode = DET_MODE_3CH_90;
    } else if (strcmp(wake_mode, "3CH_95") == 0) {
        wakenet_mode = DET_MODE_3CH_95;
    }
    free(wake_mode);

    afe_config_t cfg_afe = {
        .aec_init = config_get_bool("aec", true),
        .se_init = config_get_bool("bss", true),
        .vad_init = true,
        .wakenet_init = true,
        .voice_communication_init = false,
        .voice_communication_agc_init = false,
        .voice_communication_agc_gain = 15,
        .vad_mode = config_get_int("vad_mode", DEFAULT_VAD_MODE),
        .wakenet_mode = wakenet_mode,
        .wakenet_model_name = NULL,
        .afe_linear_gain = 1.0,
        .afe_mode = SR_MODE_HIGH_PERF,
        .afe_perferred_core = 1,
        .afe_perferred_priority = 5,
        .afe_ringbuf_size = 50,
        .memory_alloc_mode = AFE_MEMORY_ALLOC_MORE_PSRAM,
        .agc_mode = AFE_MN_PEAK_AGC_MODE_3,
        .pcm_config.total_ch_num = 3,
        .pcm_config.mic_num = 2,
        .pcm_config.ref_num = 1,
        .pcm_config.sample_rate = 16000,
        .debug_init = false,
        .debug_hook = {{AFE_DEBUG_HOOK_MASE_TASK_IN, NULL}, {AFE_DEBUG_HOOK_FETCH_TASK_IN, NULL}},
    };

    // E (5727) AFE_SR: sample_rate only support 16000, please modify it!
    // cfg_srr.afe_cfg.pcm_config.sample_rate = CFG_AUDIO_SR_SAMPLE_RATE;

    char *wake_word = config_get_char("wake_word", DEFAULT_WAKE_WORD);
    recorder_sr_cfg_t cfg_srr = {
        .afe_cfg = cfg_afe,
        .input_order = INPUT_ORDER_DEFAULT(),
        .multinet_init = false,
        .feed_task_core = FEED_TASK_PINNED_CORE,
        .feed_task_prio = FEED_TASK_PRIO,
        .feed_task_stack = FEED_TASK_STACK_SZ,
        .fetch_task_core = FETCH_TASK_PINNED_CORE,
        .fetch_task_prio = FETCH_TASK_PRIO,
        .fetch_task_stack = FETCH_TASK_STACK_SZ,
        .rb_size = 12 * 1024, // default is 6 * 1024
        .partition_label = "model",
        .mn_language = ESP_MN_ENGLISH,
        .wn_wakeword = wake_word,
    };

    ESP_LOGI(TAG, "Using record buffer '%d'", config_get_int("record_buffer", DEFAULT_RECORD_BUFFER));
    cfg_srr.rb_size = config_get_int("record_buffer", DEFAULT_RECORD_BUFFER) * 1024;

    char *speech_rec_mode = config_get_char("speech_rec_mode", DEFAULT_SPEECH_REC_MODE);
    if (strcmp(speech_rec_mode, "Multinet") == 0) {
#if defined(WILLOW_SUPPORT_MULTINET)
        esp_task_wdt_config_t twdt_config = {
            .timeout_ms = MULTINET_TWDT,
            .idle_core_mask = 0,
            .trigger_panic = CONFIG_TASK_WDT_PANIC ? true : false,
        };

        ESP_LOGI(TAG, "Using local multinet");
        ESP_LOGI(TAG, "cmd_multinet[] size: %u bytes", get_cmd_multinet_size());
#if defined(CONFIG_ESP_TASK_WDT_INIT)
        esp_task_wdt_reconfigure(&twdt_config);
#else
        esp_task_wdt_init(&twdt_config);
#endif
        cfg_srr.multinet_init = true;
        cfg_srr.rb_size = 6 * 1024;
#else
        ESP_LOGE(TAG, "multinet not supported but enabled in config");
#endif
    }
    free(speech_rec_mode);

    recorder_encoder_cfg_t recorder_encoder_cfg = {0};

    char *audio_codec = config_get_char("audio_codec", DEFAULT_AUDIO_CODEC);
    if (strcmp(audio_codec, "AMR-WB") == 0) {
        amrwb_encoder_cfg_t amrwb_cfg = DEFAULT_AMRWB_ENCODER_CONFIG();
        amrwb_cfg.contain_amrwb_header = true;
        amrwb_cfg.stack_in_ext = true;
        amrwb_cfg.task_core = 0;
        amrwb_cfg.task_prio = 5;
        amrwb_cfg.out_rb_size = 8 * 1024;
        amrwb_cfg.bitrate_mode = AMRWB_ENC_BITRATE_MD2385;

        recorder_encoder_cfg.encoder = amrwb_encoder_init(&amrwb_cfg);
    } else if (strcmp(audio_codec, "WAV") == 0) {
        wav_encoder_cfg_t wav_cfg = DEFAULT_WAV_ENCODER_CONFIG();
        wav_cfg.stack_in_ext = true;
        wav_cfg.task_core = 0;
        wav_cfg.task_prio = 5;
        wav_cfg.out_rb_size = 8 * 1024;

        recorder_encoder_cfg.encoder = wav_encoder_init(&wav_cfg);
    }

    audio_rec_cfg_t cfg_ar = {
        .pinned_core = AUDIO_REC_DEF_TASK_CORE,
        .task_prio = AUDIO_REC_DEF_TASK_PRIO,
        .task_size = AUDIO_REC_DEF_TASK_SZ,
        .event_cb = cb_ar_event,
        .user_data = NULL,
        .read = (recorder_data_read_t)&feed_afe,
        .sr_handle = NULL,
        .sr_iface = NULL,
        .wakeup_time = AUDIO_REC_DEF_WAKEUP_TM,
        .vad_start = AUDIO_REC_VAD_START_SPEECH_MS,
        .vad_off = config_get_int("vad_timeout", DEFAULT_VAD_TIMEOUT),
        .wakeup_end = 1,
        .encoder_handle = NULL,
        .encoder_iface = NULL,
    };
    cfg_ar.sr_handle = recorder_sr_create(&cfg_srr, &cfg_ar.sr_iface);
    if (strcmp(audio_codec, "AMR-WB") == 0 || strcmp(audio_codec, "WAV") == 0) {
        ESP_LOGI(TAG, "Using recorder encoder");
        cfg_ar.encoder_handle = recorder_encoder_create(&recorder_encoder_cfg, &cfg_ar.encoder_iface);
    }
    free(audio_codec);
    free(wake_word);

    if (cfg_ar.sr_handle == NULL) {
        ESP_LOGE(TAG, "failed to init SR recorder");
        ui_pr_err("Recorder init failed", "Check logs");
        return ESP_FAIL;
    }

    hdl_ar = audio_recorder_create(&cfg_ar);

    return ret;
}

static void at_read(void *data)
{
    const int len = 2 * 1024;
    char *buf = audio_calloc(len, 1);
    int msg = -1, ret = 0;
    TickType_t delay = portMAX_DELAY;

    while (true) {
        if (xQueueReceive(q_rec, &msg, delay) == pdTRUE) {
            switch (msg) {
                case MSG_START:
                    delay = 0;
                    recording = true;
                    audio_pipeline_stop(hdl_ap_to_api);
                    audio_pipeline_wait_for_stop(hdl_ap_to_api);
                    audio_pipeline_reset_ringbuffer(hdl_ap_to_api);
                    audio_pipeline_reset_elements(hdl_ap_to_api);
                    audio_pipeline_run(hdl_ap_to_api);
                    stream_to_api = true;
                    // this confirms that the URL is still set correctly
                    ESP_LOGI(TAG, "Using WIS URL '%s'", audio_element_get_uri(hdl_ae_hs));
                    __attribute__((fallthrough));
                case MSG_START_LOCAL:
                    recording = true;
                    break;
                case MSG_STOP:
                    delay = portMAX_DELAY;
                    audio_element_set_ringbuf_done(hdl_ae_rs_to_api);
                    recording = false;
                    stream_to_api = false;
                    if (lvgl_port_lock(lvgl_lock_timeout)) {
                        lv_label_set_text_static(lbl_ln3, multiwake_won ? "Thinking..." : "WOW Active - Exiting");
                        lv_obj_add_flag(btn_cancel, LV_OBJ_FLAG_HIDDEN);
                        lvgl_port_unlock();
                    }
                    reset_timer(hdl_display_timer, config_get_int("display_timeout", DEFAULT_DISPLAY_TIMEOUT), false);
                    break;
                default:
                    printf("at_read(): invalid msg '%d'\n", msg);
                    break;
            }
        }

        if (stream_to_api) {
            // printf("at_read() audio_recorder_data_read()\n");
            ret = audio_recorder_data_read(hdl_ar, buf, len, portMAX_DELAY);
            if (ret <= 0) {
                ESP_LOGD(TAG, "audio_recorder_data_read returned 0");
                // delay = portMAX_DELAY;
                // stream_to_api = false;
            } else {
                raw_stream_write(hdl_ae_rs_to_api, buf, ret);
            }
            // calling raw_stream_read twice on the same audio element "cuts" the audio in half
            // we end up sending 1 fragment to AFE and another to the API
            // ret = raw_stream_read(hdl_ae_rs_from_i2s, (char *)buf, len);
            // if (ret <= 0) {
            //     printf("at_read() ret <= 0\n");
            //     delay = portMAX_DELAY;
            //     return;
            // }
            // printf("at_read() raw_stream_write()\n");
        }
    }

    free(buf);
    vTaskDelete(NULL);
}

esp_err_t volume_set(int volume)
{
    if (volume < 0) {
        volume = config_get_int("speaker_volume", DEFAULT_SPEAKER_VOLUME);
    }
    return audio_hal_set_volume(hdl_ahc, volume);
}

void init_adc(void)
{
    if (hdl_aha != NULL) {
        hdl_aha->audio_codec_deinitialize();
    }
    hdl_aha = audio_board_adc_init();

    audio_hal_codec_i2s_iface_t cfg_i2s = {
        .mode = AUDIO_HAL_MODE_SLAVE,
        .fmt = AUDIO_HAL_I2S_NORMAL,
        .samples = AUDIO_HAL_16K_SAMPLES,
        .bits = AUDIO_HAL_BIT_LENGTH_32BITS,
    };

    hdl_aha->audio_codec_config_iface(AUDIO_HAL_CODEC_MODE_ENCODE, &cfg_i2s);
    hdl_aha->audio_codec_set_volume(config_get_int("mic_gain", DEFAULT_MIC_GAIN));
}

esp_err_t init_audio(void)
{
    char *speech_rec_mode = config_get_char("speech_rec_mode", DEFAULT_SPEECH_REC_MODE);
    char *wake_word = config_get_char("wake_word", DEFAULT_WAKE_WORD);
    esp_err_t ret = ESP_OK;

    check_mute();

    hdl_ahc = audio_board_codec_init();
    gpio_set_level(get_pa_enable_gpio(), 0);
    ret = audio_hal_ctrl_codec(hdl_ahc, AUDIO_HAL_CODEC_MODE_DECODE, AUDIO_HAL_CTRL_START);
    ESP_LOGI(TAG, "audio_hal_ctrl_codec: %s", esp_err_to_name(ret));
    init_esp_audio();
    volume_set(-1);

    if (hdl_aha == NULL) {
        init_adc();
    }

    init_audio_response();
    init_session_timer();
    if (strcmp(speech_rec_mode, "WIS") == 0) {
        init_ap_to_api();
    }
    free(speech_rec_mode);
    ESP_RETURN_ON_ERROR(start_rec(), TAG, "start_rec failed");
    hdl_aha->audio_codec_set_volume(config_get_int("mic_gain", DEFAULT_MIC_GAIN));

    ESP_LOGI(TAG, "app_main() - start_rec() finished");

    q_rec = xQueueCreate(3, sizeof(int));
    audio_thread_create(&hdl_at, "at_read", at_read, NULL, 4 * 1024, 5, true, 0);

    char wake_help[STR_WAKE_LEN] = "";
    if (strcmp(wake_word, "hiesp") == 0) {
#if defined(CONFIG_SR_WN_WN9_HIESP) || defined(CONFIG_SR_WN_WN9_HIESP_MULTI)
        strncpy(wake_help, "Say 'Hi ESP' to start!", STR_WAKE_LEN);
#endif
    } else if (strcmp(wake_word, "alexa") == 0) {
#if defined(CONFIG_SR_WN_WN9_ALEXA) || defined(CONFIG_SR_WN_WN9_ALEXA_MULTI)
        strncpy(wake_help, "Say 'Alexa' to start!", STR_WAKE_LEN);
#endif
    } else if (strcmp(wake_word, "hilexin") == 0) {
#if defined(CONFIG_SR_WN_WN9_HILEXIN) || defined(CONFIG_SR_WN_WN9_HILEXIN_MULTI)
        strncpy(wake_help, "Say 'Hi Lexin' to start!", STR_WAKE_LEN);
#endif
    }

    if (strlen(wake_help) == 0) {
        ESP_LOGE(TAG, "selected wake word (%s) not supported", wake_word);
        strncpy(wake_help, "Ready!", STR_WAKE_LEN);
    }
    free(wake_word);

    if (ld == NULL) {
        ESP_LOGE(TAG, "lv_disp_t ld is NULL!!!!");
    } else {
        if (lvgl_port_lock(lvgl_lock_timeout)) {
            lv_obj_add_flag(lbl_ln4, LV_OBJ_FLAG_HIDDEN);
            lv_label_set_long_mode(lbl_ln1, LV_LABEL_LONG_SCROLL);
            lv_label_set_long_mode(lbl_ln5, LV_LABEL_LONG_SCROLL);
            lv_label_set_text(lbl_ln3, wake_help);

            lvgl_port_unlock();
        }
    }

    return ret;
}

void deinit_audio(void)
{
    if (hdl_ar != NULL) {
        audio_recorder_destroy(hdl_ar);
    }
    if (hdl_at != NULL) {
        vTaskDelete(hdl_at);
    }
    if (hdl_ap != NULL) {
        audio_pipeline_stop(hdl_ap);
        audio_pipeline_wait_for_stop(hdl_ap);
        audio_pipeline_terminate(hdl_ap);
    }
    char *speech_rec_mode = config_get_char("speech_rec_mode", DEFAULT_SPEECH_REC_MODE);
    if (strcmp(speech_rec_mode, "WIS") && hdl_ap_to_api != NULL) {
        audio_pipeline_stop(hdl_ap_to_api);
        audio_pipeline_wait_for_stop(hdl_ap_to_api);
        audio_pipeline_terminate(hdl_ap_to_api);
    }
    free(speech_rec_mode);
    if (hdl_ea != NULL) {
        esp_audio_destroy(hdl_ea);
    }
}
