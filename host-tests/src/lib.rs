//! Host test harness for Willow's hardware-independent Rust modules.

#![allow(
    dead_code,
    reason = "production entry points are exercised by firmware while this crate executes their colocated unit tests"
)]

mod audio;
mod sr;
mod was;
