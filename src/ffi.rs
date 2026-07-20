//! Calls into the C implementation retained during the Rust migration.

mod raw {
    unsafe extern "C" {
        pub(super) fn willow_init();
        pub(super) fn willow_main_loop_iteration();
    }
}

/// Initializes the C-owned firmware subsystems.
pub(crate) fn init() {
    unsafe { raw::willow_init() };
}

/// Runs one iteration of the remaining C main loop.
pub(crate) fn main_loop_iteration() {
    unsafe { raw::willow_main_loop_iteration() };
}
