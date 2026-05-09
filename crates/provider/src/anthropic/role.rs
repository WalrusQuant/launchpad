use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// Anthropic Messages API roles supported by the wire format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AnthropicAIRole {
    User,
    Assistant,
}

impl fmt::Display for AnthropicAIRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            AnthropicAIRole::User => "user",
            AnthropicAIRole::Assistant => "assistant",
        })
    }
}

impl FromStr for AnthropicAIRole {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "user" => Ok(AnthropicAIRole::User),
            "assistant" => Ok(AnthropicAIRole::Assistant),
            other => Err(format!("invalid Anthropic role: {other}")),
        }
    }
}
