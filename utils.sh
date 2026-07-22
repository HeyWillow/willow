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

export ADF_PATH="$WILLOW_PATH/deps/esp-adf"

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

check_deps() {
    if [ ! -d deps/esp-adf ]; then
        echo "You need to run install first"
        exit 1
    fi
}

generate_nvs() {
    SSID=$(grep CONFIG_WIFI_SSID sdkconfig | cut -d'=' -f2 | tr -d '"')
    PASSWORD=$(grep CONFIG_WIFI_PASSWORD sdkconfig | cut -d'=' -f2 | tr -d '"')
    WAS_URL=$(grep CONFIG_WILLOW_WAS_URL sdkconfig | cut -d'=' -f2 | tr -d '"')
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
    cd deps
    # Setup ADF
    git clone https://github.com/espressif/esp-adf.git
    cd $ADF_PATH
    git checkout "$ADF_VER"
    git submodule update --init components/esp-adf-libs

    cd $WILLOW_PATH
    idf.py set-target "$PLATFORM"
}

destroy() {
    sudo rm -rf build serve deps target venv managed_components "$DIST_FILE" flags/*
}

# Just in case
mkdir -p flags

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
    check_deps
    idf.py menuconfig
;;

clean)
    check_container
    check_deps
    idf.py clean
;;

fullclean)
    check_container
    check_deps
    idf.py fullclean
;;

build)
    check_container
    check_deps
    if [ $2 ]; then
        echo "Adding timestamp to dev build"
        TS=$(date '+%d-%m-%Y_%H:%M:%S')
        WILLOW_VERSION+="_$TS"

    fi
    WILLOW_SDKCONFIG_SANITY_CHECKS=1 idf.py build
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

convert-font)
    lv_font_conv --format bin --lcd --bpp 4 --no-compress --size 28 --font "$2" \
        -r 0x20-0x20CF -o spiffs/ui/font.bin
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
