//! Collaboration tools as agents see them (docs/DESIGN.md §9).

use serde::{Deserialize, Serialize};

/// A tool's public description; `input_schema` is a JSON Schema object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ToolInfo {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}
