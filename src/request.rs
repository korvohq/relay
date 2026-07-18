// Copyright 2025-present Snab Limited (trading as Korvo)
// SPDX-License-Identifier: Apache-2.0

use std::collections::{BTreeSet, HashMap};

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
}

impl Message {
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct RelayRequest {
    pub messages: Vec<Message>,
    pub model: String,
    pub max_output_tokens: Option<u32>,
    #[serde(default)]
    pub stream: bool,
    #[serde(default)]
    pub metadata: HashMap<String, Value>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RelayResponse {
    pub text: String,
    pub usage: Usage,
    pub model: String,
    pub latency_ms: u64,
    pub raw: Value,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Capability {
    Chat,
    Embed,
    Consensus,
}

pub type CapabilitySet = BTreeSet<Capability>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TokenEstimate {
    pub input_tokens: u64,
}
