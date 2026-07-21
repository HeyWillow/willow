//! Calls into the C implementation retained during the Rust migration.

mod raw {
    unsafe extern "C" {
        pub(super) fn willow_init();
    }
}

/// Initializes the C-owned firmware subsystems.
pub(crate) fn init() {
    unsafe { raw::willow_init() };
}
