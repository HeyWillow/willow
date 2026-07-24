FROM espressif/idf:v5.4.4

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
COPY --chown=ubuntu:ubuntu container.gitconfig /home/ubuntu/.gitconfig

# Docker
COPY container.gitconfig /root/.gitconfig

ENV PATH="$PATH:/willow/.local/bin"
WORKDIR /willow
