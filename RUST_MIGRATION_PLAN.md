# Rust Migration Plan

## Outcome

Replace the remaining local Willow C implementation with Rust, remove ESP-ADF,
and leave `main/` containing exactly one file, `main.c`, with these complete
contents:

```c
void __attribute__((weak)) app_main(void) {}
```

No Rust code may call local Willow C code. Calls from Rust into ESP-IDF,
ESP-SR, or another external component remain permitted. Temporary calls from
the remaining C code into Rust are permitted while a caller is being migrated.

Every commit must remain buildable, small, reviewable, and build-tested.

## Dependency boundary

- ESP-SR must remain below 2.0. This migration stays on ESP-SR 1.9.5 and does
  not change the `srmodels` image or partition contract.
- PoC commit `b3c0910` is the implementation baseline. Commit `64cedce`, which
  upgrades ESP-SR, is explicitly deferred.
- An ESP-SR 2.x upgrade requires a later WAS change that can update and
  atomically activate the `srmodels` partition.
- ESP-IDF and other Espressif components may be upgraded only in isolated
  commits when there is a concrete migration benefit and the upgrade is
  proven not to require ESP-SR 2.x or change `srmodels.bin`.
- ESP-IDF 5.4.4 adds protection against an I2S DMA pointer being reused while
  blocking reads or writes fall behind. ESP-SR remains at 1.9.5, and the
  upgrade must not change `srmodels.bin`.
- Consume `willow-protocol` from its published repository through a pinned
  Cargo Git revision.

Two component upgrades are justified:

- `esp_audio_codec` 2.0.0 to 2.4.1. This adds the simple Vorbis path and fixes
  decoder reset, AMR state reset, FLAC state reset, and a corrupt-TS parser
  dead loop. See the
  [official changelog](https://components.espressif.com/components/espressif/esp_audio_codec/versions/2.4.1/changelog?language=en).
- `esp_codec_dev` 1.3.1 to 1.5.9. This is the minimum sensible target containing
  the shared RX/TX, full-duplex race, separate ADC/DAC, and ESP-IDF 5.4-or-older
  build fixes. Version 1.5.10 adds nothing relevant to Willow's hardware. See
  the
  [official changelog](https://components.espressif.com/components/espressif/esp_codec_dev/versions/1.5.10/changelog?language=en).

Both components declare only an ESP-IDF dependency, not an ESP-SR dependency:

- [ESP Audio Codec dependencies](https://components.espressif.com/components/espressif/esp_audio_codec/versions/2.4.1/dependencies?language=en)
- [ESP Codec Dev dependencies](https://components.espressif.com/components/espressif/esp_codec_dev/versions/1.5.10/dependencies?language=en)

## Existing PoC baseline

Reuse the implementation in `../willow-firmware-rs-poc` rather than creating a
new ESP-SR abstraction. In particular, retain the proven:

- ESP-SR model and AFE ownership and destruction order;
- BOX-3 microphone sample extraction;
- bounded capture, feed, and fetch loop;
- wake and VAD result interpretation; and
- ES7210 initialization and register verification.

The production integration must adapt the PoC's single reviewed model fixture
to Willow's existing model pack without changing the pack itself.

## Ownership and ordering constraints

The PoC capture path must not be enabled while ADF still owns playback. Both
directions share I2S0 clocks, and true full-duplex operation requires TX and RX
to be allocated together. Prepare and compile the Rust recorder and player
independently, then switch both directions in one explicit ownership commit.

Audio must move before the WAS callback. Moving WAS first would require Rust to
call the retained local C audio implementation, violating the intended
dependency direction.

The two existing Rust-to-local-C edges are removed late in the sequence:

1. `src/was.rs` registering `willow_was_event_handler`;
2. `src/ffi.rs` calling `willow_init`.

No additional Rust-to-local-C edge may be introduced.

## Commit sequence

### 1. Import the PoC without activating it

1. `build: guard the ESP-SR migration boundary`
2. `build: expose ESP-SR 1.9.5 bindings to Rust`
3. `sr: import the proven model and AFE owners`
4. `sr: adapt the PoC owner to the existing model pack`
5. `sr: expose fetched PCM frames safely`
6. `i2c: add codec-device access to the shared bus`
7. `audio: import the proven capture framing`
8. `audio: import the proven ES7210 initialization`
9. `audio: describe the three board audio configurations`
10. `sr: map the existing Willow AFE configuration`

All code introduced in this phase remains inactive in the running firmware.
The single-model PoC hash policy is adapted to the existing production model
pack; the pack, model selection, partition layout, and ESP-SR version do not
change.

### 2. Prepare the Rust audio engine

11. `audio: bind the current standalone codec APIs`
12. `audio: wrap the board codec devices`
13. `audio: own jointly allocated duplex I2S0 channels`
14. `audio: add bounded PCM conversion and resampling`
15. `audio: wrap stream decoders and encoders`
16. `audio: stream playback from SPIFFS`
17. `audio: stream playback over HTTP`
18. `audio: add the player worker and cancellation`
19. `audio: preserve chime and TTS response policy`
20. `audio: add PCM, WAV, and AMR-WB WIS encoding`
21. `audio: add the chunked WIS uploader`
22. `audio: model the recorder state machine`
23. `audio: connect capture, ESP-SR, and recording`
24. `audio: preserve mute, timeout, and multiwake behavior`

Playback fixtures must cover AAC, AMR-NB, AMR-WB, FLAC, M4A, MP3,
OGG/Vorbis, Opus, PCM, TS, and WAV. Pure Rust can own PCM transformation and
Ogg packetization; the standalone Espressif codec component initially
preserves the broad decoder and encoder surface.

### 3. Make the atomic audio cut-over

25. `audio: move runtime ownership to Rust`

This is the single unavoidable ownership switch:

- TX and RX move together; the PoC RX path is never enabled alongside ADF TX.
- C callers temporarily call thin Rust exports.
- `main/audio.c`, `main/audio.h`, and `main/audio_bindings.h` disappear.
- UI cancellation calls the normal Rust player API and stops carrying an
  `esp_audio_handle_t`.
- No new Rust-to-local-C call is introduced.

Detach and update dependencies separately:

26. `build: replace ADF with standalone codec components`
27. `audio: update esp_audio_codec to 2.4.1`
28. `audio: update esp_codec_dev to 1.5.9`
29. `build: remove ESP-ADF installation and tooling`

Each dependency commit must prove that ESP-SR remains 1.9.5 and that a clean
build produces the identical `srmodels.bin` for the same board configuration.

### 4. Remove the remaining C control plane

30. `config: coordinate configuration updates in Rust`

    Delete `config.c` and the string-keyed C configuration adapters.

31. `ota: coordinate upgrades in Rust`

    Delete `ota.c`. This commit does not add `srmodels` OTA support.

32. `was: model inbound server messages`
33. `was: handle wake and command results in Rust`
34. `was: handle config, NVS, restart, and OTA commands`
35. `was: run notifications in Rust`
36. `protocol: consume published willow-protocol`
37. `was: register the Rust WebSocket callback`

    Delete `was.c`, `was.h`, and `was_bindings.h`. This removes the local C
    callback edge from `src/was.rs`.

38. `main: coordinate Willow startup in Rust`

    Delete `willow_init()` and `src/ffi.rs`, removing the final
    Rust-to-local-C call.

39. `ffi: remove the retired C adapters and headers`

### 5. Produce and enforce the exact final layout

40. `build: move native build metadata out of main`

Move `Kconfig.projbuild`, the component manifest, SPIFFS image creation, and
SDK configuration sanity checks into a metadata-only component such as
`idf/willow-build/`. Configure Cargo to stage only `main/main.c` and use the
default `esp-idf-sys` main-component CMake file.

41. `main: reduce the native entry point to its weak stub`

The complete contents of `main/main.c` become:

```c
void __attribute__((weak)) app_main(void) {}
```

42. `build: enforce the Rust-only Willow boundary`

The build or CI guard checks that:

- `main/` contains exactly `main.c`;
- its contents match the weak stub;
- no ESP-ADF path is linked, staged, cloned, or installed;
- no Rust declaration references a local Willow C symbol;
- ESP-SR remains below 2.0; and
- the application still fits the 3 MiB OTA slot.

## Testing requirements

Every commit runs:

```sh
./utils.sh build
```

Additional requirements apply where relevant:

- Before native-component graph changes or C file deletions:

  ```sh
  ./utils.sh cargo clean -p esp-idf-sys
  ./utils.sh build
  ```

- Pure state-machine, protocol, PCM, WAV, resampling, and parser code receives
  host tests.
- Board-dependent commits build ESP32-S3-BOX, ESP32-S3-BOX-3, and
  ESP32-S3-BOX-Lite configurations.
- The audio cut-over and both codec-component upgrades receive capture and
  playback hardware tests on all three boards.
- Playback tests cover all currently accepted source and codec combinations,
  including cancellation, synchronous playback, repeats, and volume changes.
- WIS tests cover PCM, WAV, and AMR-WB; HTTP timeout, 401, 406, and generic
  failures; multiwake loss; session timeout; and mute/unmute.
- Startup tests cover Wi-Fi and Ethernet.
- Control-plane tests cover configuration replacement, NVS replacement,
  restart, notification cancellation, OTA success, and OTA failure.
- Component-upgrade commits compare `srmodels.bin` before and after using the
  same clean board configuration.
- The final clean release build verifies the exact `main/` contents, the lack
  of ADF references and local-C calls, and the 3 MiB application-size limit.

## Deferred work

The following work is explicitly outside this migration series:

- ESP-SR 2.x;
- changing the model image or partition layout;
- WAS protocol and storage support for `srmodels` OTA;
- atomic model-partition activation and rollback; and
- replacing the behavior-compatible codec backend with narrower pure-Rust
  decoders one format at a time.

Those changes should begin only after WAS can update the model partition
safely, and each should remain separate from the local-C removal history.
