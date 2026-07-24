#!/bin/bash
set -e # bail on error

export WILLOW_PATH=$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" &> /dev/null && pwd )
cd "$WILLOW_PATH"

export PLATFORM="esp32s3" # Current general family
export FLASH_BAUD=2000000 # Optimistic but seems to work for me for now
export CONSOLE_BAUD=2000000 # Subject to change

export DOCKER_IMAGE="willow:latest"
export DOCKER_NAME="willow-build"
export DIST_FILE="build/${dist_filename:-willow-dist.bin}"

export CARGO_HOME="$WILLOW_PATH/deps/cargo"
export RUSTUP_HOME="$WILLOW_PATH/deps/rustup"
export PATH="$CARGO_HOME/bin:$PATH"

RUST_STABLE_VERSION="1.88.0"
RUST_HOST_TARGET="x86_64-unknown-linux-gnu"
ESP_RUST_TOOLCHAIN_VERSION="1.88.0.0"
ESPUP_VERSION="0.17.1"
LDPROXY_VERSION="0.3.5"
RUST_EXPORT_FILE="$WILLOW_PATH/deps/export-esp.sh"

# Number of loops for torture test
TORTURE_LOOPS=300
# Delay in between loops
TORTURE_DELAY=3
# File to play
TORTURE_PLAY="misc/hi_esp_this_is_a_test_command.flac"

# Container or host?
# podman sets container var to podman, make docker act like that
if [ -f /.dockerenv ]; then
    export container="docker"
fi

# Get Willow version
export WILLOW_VERSION=$(git describe --always --dirty --tags)

# Test for local environment file and use any overrides
if [ -r .env ]; then
    echo "Using configuration overrides from .env file"
    . .env
fi

# Always print Willow version
echo "Willow build version: $WILLOW_VERSION"

check_port() {
    if [ ! $PORT ]; then
        echo "You need to define the PORT environment variable to do serial stuff - exiting"
        echo "If using sudo because of permissions, pass PORT with sudo. Example: sudo PORT=/dev/ttyACM0 ./utils.sh [command]"
        exit 1
    fi

    if [ ! -c $PORT ]; then
        echo "Cannot find configured port $PORT - exiting"
        exit 1
    fi

    if [ ! -w "$PORT" ]; then
        echo "You don't have permission to write to $PORT - exiting"
        echo "You need to either run this command with sudo or add yourself to the dialout group"
        echo "Example: sudo -E ./utils.sh erase-flash or sudo -E ./utils.sh flash"
        exit 1
    fi
}

check_tio() {
    if ! command -v tio &> /dev/null
    then
        echo "tio could not be found in path - you need to install it"
        echo "More information: https://github.com/tio/tio"
        exit 1
    fi
}

check_clang_format() {
    if ! command -v clang-format-15 &> /dev/null
    then
        echo "clang-format-15 could not be found in path - you need to install it"
        exit 1
    fi
}

fix_term() {
    clear
    reset
}

do_term() {
    tio -b "$CONSOLE_BAUD" "$PORT"
}

check_build_host() {
    if [ "$BUILD_HOST_PATH" ]; then
        echo "Copying build from defined remote build host and path $BUILD_HOST_PATH"
        rsync -r --delete --exclude esp-idf "$BUILD_HOST_PATH/build" .
    fi
}

check_container(){
    if [ "$container" ]; then
        return
    fi

    echo "You need to run this command inside of the container - you are on the host"
    exit 1
}

check_host(){
    if [ ! "$container" ]; then
        return
    fi

    echo "You need to run this command from the host - you are in the container"
    exit 1
}

activate_rust() {
    check_container

    if [ -r "$RUST_EXPORT_FILE" ]; then
        . "$RUST_EXPORT_FILE"
    fi
}

configure_board() {
    case "${WILLOW_BOARD:-}" in
    "")
        ;;
    m5stack-cores3|m5stack_core_s3)
        export ESP_IDF_SDKCONFIG="sdkconfig.m5stack_core_s3"
        export ESP_IDF_SDKCONFIG_DEFAULTS="sdkconfig.defaults;sdkconfig.defaults.m5stack_core_s3"
        export SDKCONFIG="$WILLOW_PATH/sdkconfig.m5stack_core_s3"
        export SDKCONFIG_DEFAULTS="$WILLOW_PATH/sdkconfig.defaults;$WILLOW_PATH/sdkconfig.defaults.m5stack_core_s3"
        ;;
    *)
        echo "Unsupported WILLOW_BOARD '$WILLOW_BOARD'"
        exit 1
        ;;
    esac
}

ensure_rust() {
    check_container

    mkdir -p "$CARGO_HOME" "$RUSTUP_HOME"

    if [ ! -x "$CARGO_HOME/bin/cargo" ]; then
        echo "Installing Rust $RUST_STABLE_VERSION into deps"
        curl --proto '=https' --tlsv1.2 -fsS https://sh.rustup.rs \
            -o "$WILLOW_PATH/deps/rustup-init.sh"
        sh "$WILLOW_PATH/deps/rustup-init.sh" \
            -y --no-modify-path --profile minimal --default-toolchain "$RUST_STABLE_VERSION"
    fi

    if [ ! -x "$CARGO_HOME/bin/espup" ]; then
        "$CARGO_HOME/bin/cargo" +"$RUST_STABLE_VERSION" install --locked \
            --target "$RUST_HOST_TARGET" --version "$ESPUP_VERSION" espup
    fi

    if ! "$CARGO_HOME/bin/rustup" toolchain list | grep -q '^esp' || [ ! -r "$RUST_EXPORT_FILE" ]; then
        "$CARGO_HOME/bin/espup" install \
            --default-host "$RUST_HOST_TARGET" \
            --export-file "$RUST_EXPORT_FILE" \
            --name esp \
            --std \
            --targets esp32s3 \
            --toolchain-version "$ESP_RUST_TOOLCHAIN_VERSION" \
            --skip-version-parse
    fi

    if [ ! -x "$CARGO_HOME/bin/ldproxy" ]; then
        "$CARGO_HOME/bin/cargo" +"$RUST_STABLE_VERSION" install --locked \
            --target "$RUST_HOST_TARGET" --version "$LDPROXY_VERSION" ldproxy
    fi

    activate_rust
}

contains_retired_adf_path() {
    local generated_root="$1"

    if [ ! -d "$generated_root" ]; then
        return 1
    fi

    [ -n "$(find "$generated_root" -mindepth 1 \
        \( -iname '*esp-adf*' -o -iname '*esp_adf*' \
        -o -iname '*audio_pipeline*' -o -iname '*audio_element*' \
        -o -iname '*esp_periph*' \) -print -quit)" ]
}

clean_retired_adf_artifacts() {
    local cargo_build_root="$WILLOW_PATH/target/xtensa-esp32s3-espidf/release/build"
    local staged_idf_root="$WILLOW_PATH/build/esp-idf"

    if contains_retired_adf_path "$cargo_build_root"; then
        echo "Retired ESP-ADF artifacts found; refreshing the esp-idf-sys build"
        "$CARGO_HOME/bin/cargo" +esp clean -p esp-idf-sys \
            --release --target xtensa-esp32s3-espidf
    fi

    if contains_retired_adf_path "$staged_idf_root"; then
        echo "Removing the obsolete staged ESP-IDF build"
        rm -rf -- "$staged_idf_root"
    fi
}

stage_cargo_build() {
    check_container

    cargo_release="$WILLOW_PATH/target/xtensa-esp32s3-espidf/release"
    cargo_elf="$cargo_release/willow"
    idf_build_dir=""

    for candidate in "$cargo_release"/build/esp-idf-sys-*/out/build; do
        if [ -d "$candidate" ]; then
            idf_build_dir="$candidate"
        fi
    done

    if [ ! -r "$cargo_elf" ] || [ ! -r "$cargo_release/bootloader.bin" ] || \
        [ ! -r "$cargo_release/partition-table.bin" ] || [ ! -r "$idf_build_dir/ota_data_initial.bin" ] || \
        [ ! -r "$idf_build_dir/srmodels/srmodels.bin" ] || [ ! -r "$idf_build_dir/user.bin" ]; then
        echo "Cargo build completed but its ESP-IDF images could not be found"
        exit 1
    fi

    mkdir -p build/bootloader build/partition_table build/srmodels
    cp "$cargo_elf" build/willow.elf
    cp "$cargo_release/bootloader.bin" build/bootloader/bootloader.bin
    cp "$cargo_release/partition-table.bin" build/partition_table/partition-table.bin
    cp "$idf_build_dir/ota_data_initial.bin" build/ota_data_initial.bin
    cp "$idf_build_dir/srmodels/srmodels.bin" build/srmodels/srmodels.bin
    cp "$idf_build_dir/user.bin" build/user.bin

    esptool.py --chip "$PLATFORM" elf2image \
        --flash_mode dio --flash_freq 80m --flash_size 16MB \
        -o build/willow.bin build/willow.elf

    app_size=$(stat -c %s build/willow.bin)
    if [ "$app_size" -gt $((0x300000)) ]; then
        echo "Application image is larger than its 3 MiB OTA partition"
        exit 1
    fi

    python3 scripts/check_rust_only_boundary.py \
        --app "$WILLOW_PATH/build/willow.bin" \
        --elf "$cargo_elf" \
        --idf-build "$idf_build_dir" \
        --staged-build "$WILLOW_PATH/build"

    printf '%s\n' \
        '--flash_mode dio' \
        '--flash_freq 80m' \
        '--flash_size 16MB' \
        '0x0 bootloader/bootloader.bin' \
        '0x8000 partition_table/partition-table.bin' \
        '0x2D000 ota_data_initial.bin' \
        '0x30000 willow.bin' \
        '0x630000 srmodels/srmodels.bin' \
        '0xC30000 user.bin' > build/flash_args

    printf '%s\n' \
        '--flash_mode dio' \
        '--flash_freq 80m' \
        '--flash_size 16MB' \
        '0x30000 willow.bin' > build/flash_app_args
}

clean_cargo_build() {
    check_container

    if [ -x "$CARGO_HOME/bin/cargo" ]; then
        activate_rust
        "$CARGO_HOME/bin/cargo" +esp clean
    fi
    rm -rf "$WILLOW_PATH/build"
}

generate_nvs() {
    local active_sdkconfig="${SDKCONFIG:-sdkconfig}"
    SSID=$(grep CONFIG_WIFI_SSID "$active_sdkconfig" | cut -d'=' -f2 | tr -d '"')
    PASSWORD=$(grep CONFIG_WIFI_PASSWORD "$active_sdkconfig" | cut -d'=' -f2 | tr -d '"')
    WAS_URL=$(grep CONFIG_WILLOW_WAS_URL "$active_sdkconfig" | cut -d'=' -f2 | tr -d '"')
    echo -n "key,type,encoding,value
WAS,namespace,,
URL,data,string,$WAS_URL
WIFI,namespace,,
PSK,data,string,$PASSWORD
SSID,data,string,$SSID" > build/nvs.csv
    /opt/esp/idf/components/nvs_flash/nvs_partition_generator/nvs_partition_gen.py generate \
        --version 2 build/nvs.csv build/nvs.bin 0x24000
}

install() {
    if [ -d deps ]; then
        echo "You already have a deps directory - exiting"
        exit 1
    fi
    mkdir -p deps
    WILLOW_CARGO_FIRST=1 idf.py set-target "$PLATFORM"
}

destroy() {
    sudo rm -rf build serve deps target venv managed_components "$DIST_FILE" flags/*
}

# Just in case
mkdir -p flags

configure_board

check_flag() {
    FLAG="$1"
    if [ ! -r flags/"$FLAG" ]; then
        echo "You need to run $FLAG first"
        exit 1
    fi
}

add_flag() {
    FLAG="$1"
    date > flags/"$FLAG"
}

remove_flag() {
    FLAG="$1"
    rm -f flags/"$FLAG"
}

do_dist() {
    cd "$WILLOW_PATH"/build
    esptool.py --chip "$PLATFORM" merge_bin -o "$WILLOW_PATH/$DIST_FILE" \
    @flash_args
    echo "Combined firmware image for flashing written"
    ls -lh "$WILLOW_PATH/$DIST_FILE"
    cd "$WILLOW_PATH"
}

# Some of this may seem redundant but for build, clean, etc we'll probably need to do our own stuff later
case $1 in

config)
    check_container
    if [ "$WILLOW_BOARD" ]; then
        WILLOW_CARGO_FIRST=1 idf.py \
            "-DSDKCONFIG=$SDKCONFIG" \
            "-DSDKCONFIG_DEFAULTS=$SDKCONFIG_DEFAULTS" \
            menuconfig
    else
        WILLOW_CARGO_FIRST=1 idf.py menuconfig
    fi
;;

clean)
    check_container
    clean_cargo_build
;;

fullclean)
    check_container
    clean_cargo_build
;;

build)
    check_container
    ensure_rust
    clean_retired_adf_artifacts
    if [ $2 ]; then
        echo "Adding timestamp to dev build"
        TS=$(date '+%d-%m-%Y_%H:%M:%S')
        WILLOW_VERSION+="_$TS"

    fi
    WILLOW_SDKCONFIG_SANITY_CHECKS=1 "$CARGO_HOME/bin/cargo" +esp build --release --locked
    stage_cargo_build
;;

# esp-idf-sys stages C component sources in its Cargo OUT_DIR. Deleted .c
# and .h files can remain there during incremental builds, so expose Cargo
# for package-scoped cleanup without requiring a full Willow rebuild.
cargo)
    check_container
    ensure_rust
    shift
    "$CARGO_HOME/bin/cargo" +esp "$@"
;;

test-rust-host)
    check_container
    ensure_rust
    "$CARGO_HOME/bin/cargo" +"$RUST_STABLE_VERSION" test \
        --manifest-path host-tests/Cargo.toml \
        --locked \
        --target "$RUST_HOST_TARGET" \
        --all-features \
        --all-targets
;;

build-docker|docker-build)
    docker build -t "$DOCKER_IMAGE" .
;;

docker)
    docker run --rm -it -v "$PWD":/willow -e TERM --name "$DOCKER_NAME" \
        "$DOCKER_IMAGE" /bin/bash
;;

flash)
    check_host
    check_port
    check_tio
    check_flag "erase-flash"
    check_build_host
    cd "$WILLOW_PATH"/build
    esptool.py --chip "$PLATFORM" -p "$PORT" -b "$FLASH_BAUD" --before default_reset --after hard_reset write_flash \
        @flash_args
    do_term
;;

flash-app)
    check_host
    check_port
    check_tio
    check_flag "erase-flash"
    check_build_host
    cd "$WILLOW_PATH"/build
    esptool.py --chip "$PLATFORM" -p "$PORT" -b "$FLASH_BAUD" --before=default_reset --after=hard_reset write_flash \
        @flash_app_args
    do_term
;;

dist)
    check_build_host
    do_dist
    generate_nvs
    dd conv=notrunc bs=1 if=build/nvs.bin of="$DIST_FILE" seek=$((0x9000))
;;

flash-dist|dist-flash)
    if [ ! -r "$DIST_FILE" ]; then
        echo "You need to run dist first"
        exit 1
    fi
    check_port
    check_build_host
    check_flag "erase-flash"
    esptool.py --chip "$PLATFORM" -p "$PORT" -b "$FLASH_BAUD" --before=default_reset --after=hard_reset write_flash \
        --flash_mode dio --flash_freq 80m --flash_size 16MB 0x0 "$WILLOW_PATH/$DIST_FILE"
    do_term
;;

erase-flash)
    check_host
    check_port
    esptool.py --chip "$PLATFORM" -p "$PORT" erase_flash
    echo "Flash erased. You will need to reflash."
    add_flag "erase-flash"
;;

monitor)
    check_host
    check_port
    check_tio
    do_term
;;

destroy)
    echo "YOU ARE ABOUT TO REMOVE THIS ENTIRE ENVIRONMENT AND RESET THE REPO. HIT ENTER TO CONFIRM."
    read
    echo "SERIOUSLY - YOU WILL LOSE WORK AND I WILL NOT STOP YOU IF YOU HIT ENTER AGAIN!"
    read
    echo "LAST CHANCE!"
    read
    destroy
    echo "Not a trace left. You will have to run setup again."
;;

install|setup)
    check_container
    install
    echo "You can now run ./utils.sh config and navigate to Willow Configuration for your environment"
;;

reinstall)
    check_container
    cp sdkconfig sdkconfig.user
    destroy
    install
    mv sdkconfig.user sdkconfig
    echo "Reinstalled with your configuration - you can either run ./utils.sh config and/or ./utils.sh build"
;;

torture)
    check_host
    echo "Running torture test for $TORTURE_LOOPS loops..."
    echo "WARNING: If testing against Tovera provided servers you will get rate-limited or blocked"

    for i in `seq 1 $TORTURE_LOOPS`; do
        echo -n "Running loop $i at" `date +"%H:%M:%S"`
        if `grep -q Raspberry /proc/cpuinfo`; then
            aplay -q "$TORTURE_PLAY"
        else
            ffplay -nodisp -hide_banner -loglevel error -autoexit -i "$TORTURE_PLAY"
        fi
    echo
    sleep $TORTURE_DELAY
    done
;;

log)
    if [ ! $2 ]; then
        echo "Need port"
        exit 1
    else
        LOG_PORT="$2"
        LOG_DEVICE="/dev/tty$LOG_PORT"
    fi

    FILE="tt-$LOG_PORT.log"

    DIR="build"
    echo "Logging device $LOG_DEVICE torture to $DIR/$FILE"
    tio -l "$DIR"/"$FILE" -b "$CONSOLE_BAUD" "$LOG_DEVICE"
;;

reset)
    if [ ! $2 ]; then
        echo "Need port"
        exit 1
    else
        PORT=/dev/tty"$2"
    fi
    esptool.py --chip esp32s3 --port "$PORT" --after hard_reset --no-stub run
;;

clang-format)
    check_clang_format
    find main/ -name '*.c' -or -name '*.h' -exec clang-format-15 -i {} +
;;

addr2line)
    shift
    xtensa-esp32s3-elf-addr2line -e /willow/build/willow.elf "$@"
;;

*)
    echo "Unknown argument - passing directly to idf.py"
    check_container
    idf.py "$@"
;;

esac
