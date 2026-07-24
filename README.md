# The Willow Inference Server has been released!

Willow users can now self-host the [Willow Inference Server](https://github.com/toverainc/willow-inference-server) for lightning-fast language inference tasks with Willow and other applications (even WebRTC) including STT, TTS, LLM, and more!

# Hello Willow Users!

Many users across various forums, social media, etc are starting to receive their hardware! I have enabled Github [discussions](https://github.com/toverainc/willow/discussions) to centralize these great conversations - stop by, introduce yourself, and let us know how things are going with Willow! Between Github discussions and issues we can all work together to make sure our early adopters have the best experience possible!

# Documentation

Visit official documentation on [heywillow.io](https://heywillow.io).

## M5Stack CoreS3

Select the CoreS3 hardware and its quad-PSRAM defaults for both configuration
and builds with `WILLOW_BOARD=m5stack-cores3`:

```sh
WILLOW_BOARD=m5stack-cores3 ./utils.sh config
WILLOW_BOARD=m5stack-cores3 ./utils.sh build
```

The generated CoreS3 configuration is kept separately from the default
ESP32-S3-BOX-3 configuration, so the two targets can retain different Wi-Fi
and Willow Application Server settings.
