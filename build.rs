//! Build-time configuration for the Willow firmware.

use std::env;
use std::fs;
use std::process;

const ESP_SR_MANIFEST_ENTRY: &str = "espressif/esp-sr: 1.9.5";
const MODEL_PARTITION: [&str; 5] = ["model", "data", "spiffs", "0x630000", "0x600000"];
const WAKE_WORD_CONFIGURATION: [&str; 4] = [
    "CONFIG_SR_WN_LOAD_MULIT_WORD=y",
    "CONFIG_SR_WN_WN9_ALEXA_MULTI=y",
    "CONFIG_SR_WN_WN9_HIESP_MULTI=y",
    "CONFIG_SR_WN_WN9_HILEXIN_MULTI=y",
];

fn main() {
    if let Err(error) = guard_sr_boundary() {
        eprintln!("ESP-SR migration boundary check failed: {error}");
        process::exit(1);
    }

    embuild::espidf::sysenv::output();

    println!("cargo:rerun-if-env-changed=WILLOW_VERSION");
    let version = match env::var("WILLOW_VERSION") {
        Ok(version) => version,
        Err(_) => "0.1".to_owned(),
    };
    println!("cargo:rustc-env=WILLOW_VERSION={version}");

    println!("cargo:rustc-check-cfg=cfg(esp_idf_esp32_s3_box_board)");
    println!("cargo:rustc-check-cfg=cfg(esp_idf_esp32_s3_box_lite_board)");
    println!("cargo:rustc-check-cfg=cfg(esp_idf_esp32_s3_box_3_board)");
    println!("cargo:rustc-check-cfg=cfg(esp_idf_mbedtls_ssl_proto_tls1_3)");
    println!("cargo:rustc-check-cfg=cfg(esp_idf_willow_debug_log)");
    println!("cargo:rustc-check-cfg=cfg(esp_idf_willow_debug_mem)");
    println!("cargo:rustc-check-cfg=cfg(esp_idf_willow_debug_runtime_stats)");
    println!("cargo:rustc-check-cfg=cfg(esp_idf_willow_debug_tasks)");
    println!("cargo:rustc-check-cfg=cfg(esp_idf_willow_debug_timers)");
    println!("cargo:rustc-check-cfg=cfg(esp_idf_willow_ethernet)");
    println!("cargo:rustc-link-arg=-Wl,--start-group");
    println!("cargo:rustc-link-arg=-lm");
    println!("cargo:rustc-link-arg=-lc");
    println!("cargo:rustc-link-arg=-Wl,--end-group");
}

fn guard_sr_boundary() -> Result<(), String> {
    const MANIFEST: &str = "main/idf_component.yml";
    const PARTITIONS: &str = "partitions_willow.csv";
    const SDKCONFIG_DEFAULTS: &str = "sdkconfig.defaults";

    for path in [MANIFEST, PARTITIONS, SDKCONFIG_DEFAULTS] {
        println!("cargo:rerun-if-changed={path}");
    }

    let manifest = read_project_file(MANIFEST)?;
    let dependencies: Vec<_> = manifest
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("espressif/esp-sr:"))
        .collect();
    if dependencies.as_slice() != [ESP_SR_MANIFEST_ENTRY] {
        return Err(format!(
            "{MANIFEST} must contain exactly `{ESP_SR_MANIFEST_ENTRY}`; found {dependencies:?}"
        ));
    }

    let partitions = read_project_file(PARTITIONS)?;
    let model_partitions: Vec<Vec<_>> = partitions
        .lines()
        .filter_map(|line| {
            let content = line
                .split_once('#')
                .map_or(line, |(content, _comment)| content);
            let fields: Vec<_> = content.split(',').map(str::trim).collect();
            (fields.first() == Some(&"model")).then_some(fields)
        })
        .collect();
    if model_partitions.as_slice() != [MODEL_PARTITION] {
        return Err(format!(
            "{PARTITIONS} must preserve model partition {MODEL_PARTITION:?}; found {model_partitions:?}"
        ));
    }

    let sdkconfig = read_project_file(SDKCONFIG_DEFAULTS)?;
    let mut wake_word_configuration: Vec<_> = sdkconfig
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("CONFIG_SR_WN_") && line.ends_with("=y"))
        .collect();
    wake_word_configuration.sort_unstable();
    if wake_word_configuration.as_slice() != WAKE_WORD_CONFIGURATION {
        return Err(format!(
            "{SDKCONFIG_DEFAULTS} must preserve wake-word selection {WAKE_WORD_CONFIGURATION:?}; found {wake_word_configuration:?}"
        ));
    }

    Ok(())
}

fn read_project_file(path: &str) -> Result<String, String> {
    fs::read_to_string(path).map_err(|error| format!("failed to read {path}: {error}"))
}
