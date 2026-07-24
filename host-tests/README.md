# Willow host tests

This dependency-free crate imports Willow's pure Rust modules from their
canonical firmware source paths. Run it from the repository root with:

```sh
./utils.sh test-rust-host
```

The firmware binary disables Cargo's test harness because its target is an
ESP32-S3. This crate makes the same unit tests executable on the build host
without copying production code or linking ESP-IDF.

The helper command also executes the in-tree `willow-protocol` tests.
