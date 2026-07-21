fn main() {
    embuild::espidf::sysenv::output();

    println!("cargo:rustc-check-cfg=cfg(esp_idf_willow_debug_log)");
    println!("cargo:rustc-check-cfg=cfg(esp_idf_willow_debug_runtime_stats)");
    println!("cargo:rustc-check-cfg=cfg(esp_idf_willow_ethernet)");
    println!("cargo:rustc-link-arg=-Wl,--start-group");
    println!("cargo:rustc-link-arg=-lm");
    println!("cargo:rustc-link-arg=-lc");
    println!("cargo:rustc-link-arg=-Wl,--end-group");
}
