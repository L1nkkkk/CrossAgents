//! Strongly typed ids. All ids are UUID v7, so they sort by creation time.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

macro_rules! define_id {
    ($($name:ident),* $(,)?) => {$(
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        #[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(type = "string"))]
        pub struct $name(Uuid);

        impl Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                self.0.serialize(s)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                Uuid::deserialize(d).map(Self)
            }
        }

        impl $name {
            pub fn new() -> Self {
                Self(Uuid::now_v7())
            }

            pub fn as_uuid(&self) -> Uuid {
                self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl From<Uuid> for $name {
            fn from(id: Uuid) -> Self {
                Self(id)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }

        impl FromStr for $name {
            type Err = uuid::Error;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Uuid::parse_str(s).map(Self)
            }
        }
    )*};
}

define_id!(AgentId, RunId, TaskId, SessionId, MessageId);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_round_trips_through_json_and_string() {
        let id = AgentId::new();
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, format!("\"{id}\""));
        assert_eq!(serde_json::from_str::<AgentId>(&json).unwrap(), id);
        assert_eq!(id.to_string().parse::<AgentId>().unwrap(), id);
    }

    #[test]
    fn ids_are_time_ordered() {
        let a = TaskId::new();
        let b = TaskId::new();
        assert!(a < b);
    }
}
