//! Per-device credentials. The public device never contains its credential hash.
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "lowercase")]
pub enum DeviceKind {
    #[default]
    Client,
    Node,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "lowercase")]
pub enum DeviceLevel {
    Read,
    #[default]
    Write,
    Admin,
}
impl DeviceLevel {
    pub fn word(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Admin => "admin",
        }
    }
}
/// The operating system a device (or the hub's own machine) runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "lowercase")]
pub enum DeviceOs {
    Windows,
    Macos,
    Linux,
    Other,
}
impl DeviceOs {
    /// Header a client sends with its device token to report its system.
    pub const HEADER: &'static str = "x-crossagents-client-os";

    pub fn word(self) -> &'static str {
        match self {
            Self::Windows => "windows",
            Self::Macos => "macos",
            Self::Linux => "linux",
            Self::Other => "other",
        }
    }
    /// Reads a reported value: empty means nothing was reported, an unknown word is `Other`.
    pub fn parse_reported(value: &str) -> Option<Self> {
        let value = value.trim();
        if value.is_empty() {
            return None;
        }
        Some(match value.to_ascii_lowercase().as_str() {
            "windows" => Self::Windows,
            "macos" => Self::Macos,
            "linux" => Self::Linux,
            _ => Self::Other,
        })
    }
    /// Maps a `std::env::consts::OS` value.
    pub fn from_std(os: &str) -> Self {
        match os {
            "windows" => Self::Windows,
            "macos" => Self::Macos,
            "linux" => Self::Linux,
            _ => Self::Other,
        }
    }
    /// The system this process runs on.
    pub fn current() -> Self {
        Self::from_std(std::env::consts::OS)
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Device {
    pub id: String,
    pub name: String,
    pub kind: DeviceKind,
    pub level: DeviceLevel,
    pub created_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub last_seen_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub revoked_at: Option<DateTime<Utc>>,
    /// The system the device last reported (`x-crossagents-client-os`); absent until it does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub os: Option<DeviceOs>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reported_os_words() {
        assert_eq!(DeviceOs::parse_reported("windows"), Some(DeviceOs::Windows));
        assert_eq!(DeviceOs::parse_reported(" MacOS "), Some(DeviceOs::Macos));
        assert_eq!(DeviceOs::parse_reported("linux"), Some(DeviceOs::Linux));
        assert_eq!(DeviceOs::parse_reported("freebsd"), Some(DeviceOs::Other));
        assert_eq!(DeviceOs::parse_reported("  "), None);
        assert_eq!(DeviceOs::from_std("macos"), DeviceOs::Macos);
        assert_eq!(DeviceOs::from_std("android"), DeviceOs::Other);
        assert_eq!(serde_json::to_value(DeviceOs::Macos).unwrap(), "macos");
    }
}
