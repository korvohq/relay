use std::{time::Duration, time::Instant};

use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{
    adapters::{
        ProviderAdapter, capabilities_chat, conservative_token_estimate, safe_provider_message,
    },
    error::{RelayError, Result},
    request::{CapabilitySet, RelayRequest, RelayResponse, Role, TokenEstimate, Usage},
};

pub struct AnthropicAdapter {
    client: Client,
    api_key: String,
    endpoint: String,
}

impl AnthropicAdapter {
    pub fn new(api_key: String, timeout: Duration) -> Result<Self> {
        Self::with_endpoint(
            api_key,
            timeout,
            "https://api.anthropic.com/v1/messages".into(),
        )
    }

    pub fn with_endpoint(api_key: String, timeout: Duration, endpoint: String) -> Result<Self> {
        Ok(Self {
            client: Client::builder().timeout(timeout).build()?,
            api_key,
            endpoint,
        })
    }
}

#[derive(Serialize)]
struct AnthropicRequest<'a> {
    model: &'a str,
    messages: Vec<AnthropicMessage<'a>>,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    stream: bool,
}

#[derive(Serialize)]
struct AnthropicMessage<'a> {
    role: &'static str,
    content: &'a str,
}

#[derive(Deserialize)]
struct AnthropicResponse {
    id: Option<String>,
    model: Option<String>,
    content: Vec<AnthropicContent>,
    usage: Option<AnthropicUsage>,
    stop_reason: Option<String>,
}

#[derive(Deserialize)]
struct AnthropicContent {
    #[serde(rename = "type")]
    kind: String,
    text: Option<String>,
}

#[derive(Deserialize)]
struct AnthropicUsage {
    input_tokens: u64,
    output_tokens: u64,
}

#[async_trait]
impl ProviderAdapter for AnthropicAdapter {
    fn name(&self) -> &'static str {
        "anthropic"
    }

    fn capabilities(&self) -> CapabilitySet {
        capabilities_chat()
    }

    fn estimate_tokens(&self, request: &RelayRequest) -> Result<TokenEstimate> {
        conservative_token_estimate(request)
    }

    async fn complete(&self, request: &RelayRequest) -> Result<RelayResponse> {
        if request.stream {
            return Err(RelayError::StreamingUnsupported);
        }
        let system_messages: Vec<&str> = request
            .messages
            .iter()
            .filter(|message| message.role == Role::System)
            .map(|message| message.content.as_str())
            .collect();
        let system = (!system_messages.is_empty()).then(|| system_messages.join("\n\n"));
        let messages = request
            .messages
            .iter()
            .filter_map(|message| {
                let role = match message.role {
                    Role::System => return None,
                    Role::User => "user",
                    Role::Assistant => "assistant",
                };
                Some(AnthropicMessage {
                    role,
                    content: &message.content,
                })
            })
            .collect();

        let started = Instant::now();
        let response = self
            .client
            .post(&self.endpoint)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .json(&AnthropicRequest {
                model: &request.model,
                messages,
                max_tokens: request.max_output_tokens.unwrap_or(1024),
                system,
                stream: false,
            })
            .send()
            .await?;
        let status = response.status();
        let body = response.text().await?;
        if !status.is_success() {
            return Err(RelayError::Provider {
                provider: self.name().into(),
                status: status.as_u16(),
                message: safe_provider_message(&body),
            });
        }

        let payload: AnthropicResponse = serde_json::from_str(&body)?;
        let usage = payload.usage.ok_or_else(|| {
            RelayError::InvalidUsage("Anthropic response omitted the usage object".into())
        })?;
        let text: String = payload
            .content
            .iter()
            .filter(|item| item.kind == "text")
            .filter_map(|item| item.text.as_deref())
            .collect();
        if text.is_empty() {
            return Err(RelayError::InvalidUsage(
                "Anthropic response contained no text".into(),
            ));
        }
        let model = payload.model.unwrap_or_else(|| request.model.clone());
        Ok(RelayResponse {
            text,
            usage: Usage {
                input_tokens: usage.input_tokens,
                output_tokens: usage.output_tokens,
            },
            model,
            latency_ms: started.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
            raw: json!({
                "id": payload.id,
                "stop_reason": payload.stop_reason,
            }),
        })
    }
}
