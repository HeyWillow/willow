//! Hardware-independent audio state, framing, parsing, and conversion tests.

#[path = "../../../src/audio/capture.rs"]
mod capture;
#[path = "../../../src/audio/http_audio.rs"]
mod http_audio;
#[path = "../../../src/audio/http_chunk.rs"]
mod http_chunk;
#[path = "../../../src/audio/ogg_headers.rs"]
mod ogg_headers;
#[path = "../../../src/audio/pcm.rs"]
mod pcm;
#[path = "../../../src/audio/record_buffer.rs"]
mod record_buffer;
#[path = "../../../src/audio/recorder_credit.rs"]
mod recorder_credit;
#[path = "../../../src/audio/recorder_state.rs"]
mod recorder_state;
#[path = "../../../src/audio/recorder_timing.rs"]
mod recorder_timing;
#[path = "../../../src/audio/response.rs"]
mod response;
#[path = "../../../src/audio/spiffs_uri.rs"]
mod spiffs_uri;
#[path = "../../../src/audio/wis_framing.rs"]
mod wis_framing;
