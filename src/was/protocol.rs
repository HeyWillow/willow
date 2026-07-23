//! Provisional WAS wire types, intended for extraction to `willow-protocol`.

use serde::Serialize;

/// Commands sent from Willow to the application server.
#[derive(Serialize)]
#[serde(rename_all = "snake_case", tag = "cmd")]
pub(super) enum Command<T> {
    GetConfig,
    Endpoint { data: T },
}

/// Events sent from Willow to the application server.
#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Event {
    WakeStart { wake_volume: f32 },
}
