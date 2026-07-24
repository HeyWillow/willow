//! Provisional WAS wire types, intended for extraction to `willow-protocol`.

use serde::{Deserialize, Deserializer, Serialize, de::DeserializeOwned, de::Error as _};
use serde_json::{Map, Value};

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
    NotifyDone(u64),
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

/// One message received from the Willow Application Server.
#[derive(Debug, Deserialize, PartialEq)]
pub(super) struct InboundMessage {
    #[serde(default, deserialize_with = "optional_object")]
    pub(super) wake_result: Option<WakeResult>,
    #[serde(default, deserialize_with = "optional_object")]
    pub(super) result: Option<CommandResult>,
    #[serde(default, deserialize_with = "optional_map")]
    pub(super) config: Option<Map<String, Value>>,
    #[serde(default, deserialize_with = "optional_map")]
    pub(super) nvs: Option<Map<String, Value>>,
    #[serde(default, rename = "cmd", deserialize_with = "optional_command")]
    pub(super) command: Option<InboundCommand>,
    #[serde(default, rename = "data", deserialize_with = "optional_object")]
    pub(super) notification: Option<Notification>,
    #[serde(default, deserialize_with = "optional_string")]
    pub(super) ota_url: Option<String>,
}

/// Result of Willow One Wake arbitration.
#[derive(Debug, Deserialize, PartialEq)]
pub(super) struct WakeResult {
    #[serde(default, deserialize_with = "optional_bool")]
    pub(super) won: Option<bool>,
}

/// Result of a command handled by the application server.
#[derive(Debug, Deserialize, PartialEq)]
pub(super) struct CommandResult {
    #[serde(default, deserialize_with = "optional_bool")]
    pub(super) ok: Option<bool>,
    #[serde(default, deserialize_with = "optional_string")]
    pub(super) speech: Option<String>,
}

/// Commands sent by the application server.
#[derive(Debug, PartialEq)]
pub(super) enum InboundCommand {
    Notify,
    Identify,
    OtaStart,
    Restart,
    Unknown(String),
}

/// Parameters for a notification command.
#[derive(Debug, Deserialize, PartialEq)]
pub(super) struct Notification {
    #[serde(default, deserialize_with = "optional_u64")]
    pub(super) id: Option<u64>,
    #[serde(default, deserialize_with = "optional_bool")]
    pub(super) cancel: Option<bool>,
    #[serde(default, deserialize_with = "optional_string")]
    pub(super) audio_url: Option<String>,
    #[serde(default, deserialize_with = "optional_string")]
    pub(super) text: Option<String>,
    #[serde(default, deserialize_with = "optional_i32")]
    pub(super) repeat: Option<i32>,
    #[serde(default, deserialize_with = "optional_bool")]
    pub(super) backlight: Option<bool>,
    #[serde(default, deserialize_with = "optional_bool")]
    pub(super) backlight_max: Option<bool>,
    #[serde(default, deserialize_with = "optional_i32")]
    pub(super) strobe_period_ms: Option<i32>,
    #[serde(default, deserialize_with = "optional_i32")]
    pub(super) volume: Option<i32>,
}

fn optional_object<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: DeserializeOwned,
{
    let value = Value::deserialize(deserializer)?;
    if !value.is_object() {
        return Ok(None);
    }

    T::deserialize(value).map(Some).map_err(D::Error::custom)
}

fn optional_map<'de, D>(deserializer: D) -> Result<Option<Map<String, Value>>, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(match Value::deserialize(deserializer)? {
        Value::Object(value) => Some(value),
        _ => None,
    })
}

fn optional_command<'de, D>(deserializer: D) -> Result<Option<InboundCommand>, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(Value::deserialize(deserializer)?
        .as_str()
        .map(|command| match command {
            "notify" => InboundCommand::Notify,
            "identify" => InboundCommand::Identify,
            "ota_start" => InboundCommand::OtaStart,
            "restart" => InboundCommand::Restart,
            command => InboundCommand::Unknown(command.to_owned()),
        }))
}

fn optional_bool<'de, D>(deserializer: D) -> Result<Option<bool>, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(Value::deserialize(deserializer)?.as_bool())
}

fn optional_string<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(Value::deserialize(deserializer)?
        .as_str()
        .map(str::to_owned))
}

// cJSON exposes every JSON number through a double and truncates it when the
// C handler reads `valueint`; retain that wire behavior during migration.
#[allow(clippy::cast_possible_truncation)]
fn optional_i32<'de, D>(deserializer: D) -> Result<Option<i32>, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(Value::deserialize(deserializer)?
        .as_f64()
        .map(|value| value as i32))
}

// Notification IDs previously used cJSON's double and a direct uint64_t
// conversion. Rust's saturating float cast makes that conversion defined.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn optional_u64<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(Value::deserialize(deserializer)?
        .as_f64()
        .map(|value| value as u64))
}

#[cfg(test)]
mod tests {
    #[test]
    fn serializes_notification_completion() {
        let event = serde_json::to_string(&super::Event::NotifyDone(42))
            .expect("serializable notification completion");

        assert_eq!(event, r#"{"notify_done":42}"#);
    }

    #[test]
    fn models_each_top_level_message_shape_together() {
        let message: super::InboundMessage = serde_json::from_str(
            r#"{
                "wake_result":{"won":false},
                "result":{"ok":true,"speech":"hello"},
                "config":{"multiwake":true},
                "nvs":{"WIFI":{"SSID":"willow"}},
                "cmd":"ota_start",
                "ota_url":"https://example.com/willow.bin"
            }"#,
        )
        .expect("valid inbound message");

        assert_eq!(
            message.wake_result.and_then(|result| result.won),
            Some(false)
        );
        let result = message.result.expect("command result");
        assert_eq!(result.ok, Some(true));
        assert_eq!(result.speech.as_deref(), Some("hello"));
        assert!(message.config.is_some());
        assert!(message.nvs.is_some());
        assert_eq!(message.command, Some(super::InboundCommand::OtaStart));
        assert_eq!(
            message.ota_url.as_deref(),
            Some("https://example.com/willow.bin")
        );
    }

    #[test]
    fn ignores_values_of_the_wrong_wire_type() {
        let message: super::InboundMessage = serde_json::from_str(
            r#"{
                "wake_result":true,
                "result":{"ok":"yes","speech":1},
                "config":[],
                "nvs":"invalid",
                "cmd":5,
                "data":false,
                "ota_url":{}
            }"#,
        )
        .expect("tolerated inbound message");

        assert!(message.wake_result.is_none());
        let result = message.result.expect("result object");
        assert!(result.ok.is_none());
        assert!(result.speech.is_none());
        assert!(message.config.is_none());
        assert!(message.nvs.is_none());
        assert!(message.command.is_none());
        assert!(message.notification.is_none());
        assert!(message.ota_url.is_none());
    }

    #[test]
    fn models_notification_values_before_applying_defaults() {
        let message: super::InboundMessage = serde_json::from_str(
            r#"{
                "cmd":"notify",
                "data":{
                    "id":42.9,
                    "cancel":true,
                    "audio_url":"https://example.com/alert.flac",
                    "text":"Hello",
                    "repeat":2.9,
                    "backlight":false,
                    "backlight_max":false,
                    "strobe_period_ms":250,
                    "volume":75
                }
            }"#,
        )
        .expect("valid notification");

        assert_eq!(message.command, Some(super::InboundCommand::Notify));
        let notification = message.notification.expect("notification data");
        assert_eq!(notification.id, Some(42));
        assert_eq!(notification.cancel, Some(true));
        assert_eq!(
            notification.audio_url.as_deref(),
            Some("https://example.com/alert.flac")
        );
        assert_eq!(notification.text.as_deref(), Some("Hello"));
        assert_eq!(notification.repeat, Some(2));
        assert_eq!(notification.backlight, Some(false));
        assert_eq!(notification.backlight_max, Some(false));
        assert_eq!(notification.strobe_period_ms, Some(250));
        assert_eq!(notification.volume, Some(75));
    }

    #[test]
    fn retains_unknown_command_names() {
        let message: super::InboundMessage =
            serde_json::from_str(r#"{"cmd":"future_command"}"#).expect("valid unknown command");

        assert_eq!(
            message.command,
            Some(super::InboundCommand::Unknown("future_command".to_owned()))
        );
    }
}
