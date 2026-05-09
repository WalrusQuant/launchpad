use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// OpenAI chat-completion roles supported by the wire format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OpenAIRole {
    System,
    Developer,
    User,
    Assistant,
    Tool,
    Function,
}

impl fmt::Display for OpenAIRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            OpenAIRole::System => "system",
            OpenAIRole::Developer => "developer",
            OpenAIRole::User => "user",
            OpenAIRole::Assistant => "assistant",
            OpenAIRole::Tool => "tool",
            OpenAIRole::Function => "function",
        })
    }
}

impl FromStr for OpenAIRole {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "system" => Ok(OpenAIRole::System),
            "developer" => Ok(OpenAIRole::Developer),
            "user" => Ok(OpenAIRole::User),
            "assistant" => Ok(OpenAIRole::Assistant),
            "tool" => Ok(OpenAIRole::Tool),
            "function" => Ok(OpenAIRole::Function),
            other => Err(format!("invalid OpenAI role: {other}")),
        }
    }
}
