fn main() {
    embuild::espidf::sysenv::output();

    println!("cargo:rustc-link-arg=-Wl,--start-group");
    println!("cargo:rustc-link-arg=-lm");
    println!("cargo:rustc-link-arg=-lc");
    println!("cargo:rustc-link-arg=-Wl,--end-group");
}
