FROM espressif/idf:v5.3.4

ARG DEBIAN_FRONTEND="noninteractive"

RUN apt-get -qq update && \
    apt-get -qq install \
    clang-format-15 \
    git \
    libusb-1.0-0 \
    nano \
    python-is-python3 \
    python3 \
    python3-num2words \
    python3-pip \
    python3-requests \
    python3-venv \
    sudo \
    tio \
    && rm -rf /var/lib/apt/lists/*

# Podman
RUN useradd --create-home --uid 1000 build
COPY --chown=1000 container.gitconfig /home/build/.gitconfig

# Docker
COPY container.gitconfig /root/.gitconfig

ENV PATH="$PATH:/willow/.local/bin"
WORKDIR /willow
