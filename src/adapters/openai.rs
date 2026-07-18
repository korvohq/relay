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
    request::{CapabilitySet, Message, RelayRequest, RelayResponse, TokenEstimate, Usage},
};

pub struct OpenAiAdapter {
    client: Client,
    api_key: String,
    endpoint: String,
}

impl OpenAiAdapter {
    pub fn new(api_key: String, timeout: Duration) -> Result<Self> {
        Self::with_endpoint(
            api_key,
            timeout,
            "https://api.openai.com/v1/chat/completions".into(),
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
struct OpenAiRequest<'a> {
    model: &'a str,
    messages: &'a [Message],
    max_tokens: Option<u32>,
    stream: bool,
}

#[derive(Deserialize)]
struct OpenAiResponse {
    id: Option<String>,
    model: Option<String>,
    choices: Vec<OpenAiChoice>,
    usage: Option<OpenAiUsage>,
}

#[derive(Deserialize)]
struct OpenAiChoice {
    message: OpenAiMessage,
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct OpenAiMessage {
    content: Option<String>,
}

#[derive(Deserialize)]
struct OpenAiUsage {
    prompt_tokens: u64,
    completion_tokens: u64,
}

#[async_trait]
impl ProviderAdapter for OpenAiAdapter {
    fn name(&self) -> &'static str {
        "openai"
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
        let started = Instant::now();
        let response = self
            .client
            .post(&self.endpoint)
            .bearer_auth(&self.api_key)
            .json(&OpenAiRequest {
                model: &request.model,
                messages: &request.messages,
                max_tokens: request.max_output_tokens,
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

        let payload: OpenAiResponse = serde_json::from_str(&body)?;
        let usage = payload.usage.ok_or_else(|| {
            RelayError::InvalidUsage("OpenAI response omitted the usage object".into())
        })?;
        let choice = payload
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| RelayError::InvalidUsage("OpenAI response had no choices".into()))?;
        let text = choice.message.content.ok_or_else(|| {
            RelayError::InvalidUsage("OpenAI response choice contained no text".into())
        })?;
        let model = payload.model.unwrap_or_else(|| request.model.clone());
        Ok(RelayResponse {
            text,
            usage: Usage {
                input_tokens: usage.prompt_tokens,
                output_tokens: usage.completion_tokens,
            },
            model,
            latency_ms: started.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
            raw: json!({
                "id": payload.id,
                "finish_reason": choice.finish_reason,
            }),
        })
    }
}
