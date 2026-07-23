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
    Goodbye(DeviceIdentity),
    Hello(DeviceIdentity),
    WakeEnd {},
    WakeStart { wake_volume: f32 },
}

/// Identity included when a Willow connects to or leaves the server.
#[derive(Serialize)]
pub(super) struct DeviceIdentity {
    hostname: String,
    #[serde(rename = "hw_type")]
    hardware: String,
    mac_addr: [u8; 6],
}

impl DeviceIdentity {
    pub(super) fn new(hostname: String, hardware: &str, mac_addr: [u8; 6]) -> Self {
        Self {
            hostname,
            hardware: hardware.to_owned(),
            mac_addr,
        }
    }
}
