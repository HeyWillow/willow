#include "audio_recorder.h"
#include "esp_audio.h"

#include "audio_bindings.h"

#define DEFAULT_WAKE_CONFIRMATION false

struct willow_audio_response {
    void (*fn_err)(void *data);
    void (*fn_ok)(void *data);
};

extern audio_rec_handle_t hdl_ar;
extern _Atomic bool multiwake_won;
extern esp_audio_handle_t hdl_ea;
extern QueueHandle_t q_rec;
extern struct willow_audio_response war;

void deinit_audio(void);
esp_err_t init_audio(void);
void play_audio_ok(void *data);
esp_err_t volume_set(int volume);
