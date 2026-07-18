// Copyright 2025-present Snab Limited (trading as Korvo)
// SPDX-License-Identifier: Apache-2.0

mod anthropic;
mod openai;

use std::{sync::Arc, time::Duration};

use async_trait::async_trait;

use crate::{
    error::{RelayError, Result},
    request::{CapabilitySet, RelayRequest, RelayResponse, TokenEstimate},
};

pub use anthropic::AnthropicAdapter;
pub use openai::OpenAiAdapter;

#[async_trait]
pub trait ProviderAdapter: Send + Sync {
    fn name(&self) -> &'static str;
    fn capabilities(&self) -> CapabilitySet;
    fn estimate_tokens(&self, request: &RelayRequest) -> Result<TokenEstimate>;
    async fn complete(&self, request: &RelayRequest) -> Result<RelayResponse>;
}

pub fn adapter_for(provider: &str, api_key: String) -> Result<Arc<dyn ProviderAdapter>> {
    let timeout = Duration::from_secs(120);
    match provider {
        "openai" => Ok(Arc::new(OpenAiAdapter::new(api_key, timeout)?)),
        "anthropic" => Ok(Arc::new(AnthropicAdapter::new(api_key, timeout)?)),
        _ => Err(RelayError::UnsupportedProvider(provider.to_owned())),
    }
}

pub(crate) fn conservative_token_estimate(request: &RelayRequest) -> Result<TokenEstimate> {
    if request.stream {
        return Err(RelayError::StreamingUnsupported);
    }
    let content_bytes = request
        .messages
        .iter()
        .try_fold(0_u64, |total, message| {
            total
                .checked_add(message.content.len() as u64)
                .and_then(|value| value.checked_add(16))
        })
        .ok_or_else(|| RelayError::InvalidUsage("request size overflow".into()))?;
    let input_tokens = content_bytes
        .checked_add(16)
        .ok_or_else(|| RelayError::InvalidUsage("request size overflow".into()))?;
    Ok(TokenEstimate { input_tokens })
}

pub(crate) fn capabilities_chat() -> CapabilitySet {
    [crate::request::Capability::Chat].into_iter().collect()
}

pub(crate) fn safe_provider_message(body: &str) -> String {
    let value = serde_json::from_str::<serde_json::Value>(body).ok();
    let message = value
        .as_ref()
        .and_then(|json| json.pointer("/error/message"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("provider request failed");
    message.chars().take(500).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::request::Message;

    #[test]
    fn estimate_is_deterministic_and_conservative() {
        let request = RelayRequest {
            messages: vec![Message::user("hello")],
            model: "model".into(),
            max_output_tokens: Some(10),
            stream: false,
            metadata: Default::default(),
        };
        let first = conservative_token_estimate(&request).unwrap();
        let second = conservative_token_estimate(&request).unwrap();
        assert_eq!(first, second);
        assert!(first.input_tokens >= 5);
    }

    #[test]
    fn streaming_is_rejected_before_transport() {
        let request = RelayRequest {
            stream: true,
            ..Default::default()
        };
        assert!(matches!(
            conservative_token_estimate(&request),
            Err(RelayError::StreamingUnsupported)
        ));
    }
}
