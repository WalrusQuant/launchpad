use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GoogleRole {
    User,
    Model,
}

impl fmt::Display for GoogleRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            GoogleRole::User => "user",
            GoogleRole::Model => "model",
        })
    }
}

impl FromStr for GoogleRole {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "user" | "system" | "developer" => Ok(GoogleRole::User),
            "model" | "assistant" => Ok(GoogleRole::Model),
            other => Err(format!("invalid Google role: {other}")),
        }
    }
}
