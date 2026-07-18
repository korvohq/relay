use std::sync::Arc;

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::{
    adapters::{ProviderAdapter, adapter_for},
    config::{AppPaths, Config},
    error::{RelayError, Result},
    ledger::{CallRecord, Ledger, Period, UsageSummary},
    pricing::{PriceCatalog, PriceEntry},
    request::{Message, RelayRequest, RelayResponse},
};

const DEFAULT_MAX_OUTPUT_TOKENS: u32 = 1024;

pub struct RelayService {
    pub paths: AppPaths,
    pub config: Config,
    pub prices: PriceCatalog,
    pub ledger: Ledger,
}

pub struct CompletedCall {
    pub response: RelayResponse,
    pub cost_microusd: i64,
    pub canonical_model: String,
    pub warning: Option<String>,
}

impl RelayService {
    pub fn load(paths: AppPaths) -> Result<Self> {
        let config = Config::load_or_create(&paths)?;
        let prices = PriceCatalog::load(&paths.prices)?;
        let ledger = Ledger::open(paths.ledger.clone())?;
        Ok(Self {
            paths,
            config,
            prices,
            ledger,
        })
    }

    pub async fn ask(
        &self,
        prompt: String,
        requested_model: Option<&str>,
        think: bool,
        max_output_tokens: Option<u32>,
    ) -> Result<CompletedCall> {
        let canonical_model = self.config.resolve_model(requested_model, think)?;
        let provider = split_model(&canonical_model)?.0.to_owned();
        let price = self.prices.get_verified(&canonical_model)?;
        let api_key = self.config.credential(&provider)?;
        let adapter = adapter_for(&provider, api_key)?;
        let request = RelayRequest {
            messages: vec![Message::user(prompt)],
            model: price.api_model.clone(),
            max_output_tokens: Some(max_output_tokens.unwrap_or(DEFAULT_MAX_OUTPUT_TOKENS)),
            stream: false,
            metadata: Default::default(),
        };
        self.execute_with_adapter(canonical_model, provider, request, adapter, Utc::now())
            .await
    }

    pub async fn execute_with_adapter(
        &self,
        canonical_model: String,
        provider: String,
        request: RelayRequest,
        adapter: Arc<dyn ProviderAdapter>,
        now: DateTime<Utc>,
    ) -> Result<CompletedCall> {
        if adapter.name() != provider {
            return Err(RelayError::UnsupportedProvider(format!(
                "adapter '{}' cannot serve provider '{provider}'",
                adapter.name()
            )));
        }
        if request.stream {
            return Err(RelayError::StreamingUnsupported);
        }
        let price = self.prices.get_verified(&canonical_model)?.clone();
        let estimate = adapter.estimate_tokens(&request)?;
        let max_output_tokens = request
            .max_output_tokens
            .unwrap_or(DEFAULT_MAX_OUTPUT_TOKENS) as u64;
        let requested_tokens = estimate
            .input_tokens
            .checked_add(max_output_tokens)
            .ok_or_else(|| RelayError::InvalidUsage("token estimate overflow".into()))?;
        if requested_tokens > price.max_context {
            return Err(RelayError::ContextExceeded {
                requested_tokens,
                max_context: price.max_context,
            });
        }

        let worst_case = price.cost_microusd(estimate.input_tokens, max_output_tokens)?;
        let caps = self.config.cap_limits()?;
        let reservation =
            self.ledger
                .reserve(now, caps, worst_case, &provider, &canonical_model)?;

        self.ledger.mark_pending(
            &reservation.id,
            "provider dispatch began; response has not yet been settled",
        )?;
        let response = match adapter.complete(&request).await {
            Ok(response) => response,
            Err(error) => {
                if is_confirmed_unbilled_rejection(&error) {
                    self.ledger.release(&reservation.id)?;
                    return Err(error);
                }
                return Err(RelayError::PendingReconciliation {
                    reservation_id: reservation.id,
                    reason: error.to_string(),
                });
            }
        };

        if let Err(error) = validate_usage(&response, &price) {
            return Err(RelayError::PendingReconciliation {
                reservation_id: reservation.id,
                reason: error.to_string(),
            });
        }
        let actual_cost =
            match price.cost_microusd(response.usage.input_tokens, response.usage.output_tokens) {
                Ok(cost) => cost,
                Err(error) => {
                    return Err(RelayError::PendingReconciliation {
                        reservation_id: reservation.id,
                        reason: error.to_string(),
                    });
                }
            };
        if let Err(error) = self.ledger.settle(
            &reservation.id,
            &CallRecord {
                ts: now,
                provider,
                model: canonical_model.clone(),
                tokens_in: response.usage.input_tokens,
                tokens_out: response.usage.output_tokens,
                cost_microusd: actual_cost,
                latency_ms: response.latency_ms,
                session_id: Uuid::new_v4().to_string(),
                route_tier: "direct".into(),
                deflected: false,
                price_input_per_mtok: price.input_per_mtok.to_string(),
                price_output_per_mtok: price.output_per_mtok.to_string(),
            },
            caps,
        ) {
            return Err(RelayError::PendingReconciliation {
                reservation_id: reservation.id,
                reason: format!("response received but ledger settlement failed: {error}"),
            });
        }

        let usage = self.ledger.usage(now, caps, Period::Day)?;
        let month = self.ledger.usage(now, caps, Period::Month)?;
        let warning = cap_warning(usage.total_microusd, caps.daily_microusd, "daily")
            .or_else(|| cap_warning(month.total_microusd, caps.monthly_microusd, "monthly"));
        Ok(CompletedCall {
            response,
            cost_microusd: actual_cost,
            canonical_model,
            warning,
        })
    }

    pub fn usage(&self, period: Period) -> Result<UsageSummary> {
        self.ledger
            .usage(Utc::now(), self.config.cap_limits()?, period)
    }

    pub fn update_caps(
        &mut self,
        daily_usd: Option<String>,
        monthly_usd: Option<String>,
    ) -> Result<()> {
        if daily_usd.is_none() && monthly_usd.is_none() {
            return Err(RelayError::Config(
                "provide --daily-usd and/or --monthly-usd".into(),
            ));
        }
        if let Some(value) = daily_usd {
            self.config.caps.daily_usd = value;
        }
        if let Some(value) = monthly_usd {
            self.config.caps.monthly_usd = value;
        }
        self.config.save(&self.paths.config)
    }
}

fn split_model(model: &str) -> Result<(&str, &str)> {
    let (provider, name) = model
        .split_once(':')
        .ok_or_else(|| RelayError::UnknownModel(model.to_owned()))?;
    if provider.is_empty() || name.is_empty() || name.contains(':') {
        return Err(RelayError::UnknownModel(model.to_owned()));
    }
    Ok((provider, name))
}

fn validate_usage(response: &RelayResponse, price: &PriceEntry) -> Result<()> {
    let total = response
        .usage
        .input_tokens
        .checked_add(response.usage.output_tokens)
        .ok_or_else(|| RelayError::InvalidUsage("provider token total overflow".into()))?;
    if total > price.max_context {
        return Err(RelayError::InvalidUsage(format!(
            "provider reported {total} tokens for a {}-token model context",
            price.max_context
        )));
    }
    Ok(())
}

fn is_confirmed_unbilled_rejection(error: &RelayError) -> bool {
    matches!(
        error,
        RelayError::Provider {
            status: 400..=499,
            ..
        }
    )
}

fn cap_warning(spent: i64, cap: i64, period: &str) -> Option<String> {
    if cap > 0 && spent.saturating_mul(100) >= cap.saturating_mul(80) {
        Some(format!(
            "warning: {period} spend has reached at least 80% of its configured cap"
        ))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{BTreeMap, BTreeSet},
        str::FromStr,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use async_trait::async_trait;
    use chrono::TimeZone;
    use rust_decimal::Decimal;
    use tempfile::tempdir;

    use super::*;
    use crate::{
        config::{CapsConfig, ProviderConfig},
        pricing::PriceEntry,
        request::{Capability, TokenEstimate, Usage},
    };

    struct FakeAdapter {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl ProviderAdapter for FakeAdapter {
        fn name(&self) -> &'static str {
            "test"
        }
        fn capabilities(&self) -> crate::request::CapabilitySet {
            BTreeSet::from([Capability::Chat])
        }
        fn estimate_tokens(&self, _request: &RelayRequest) -> Result<TokenEstimate> {
            Ok(TokenEstimate { input_tokens: 10 })
        }
        async fn complete(&self, request: &RelayRequest) -> Result<RelayResponse> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(RelayResponse {
                text: "ok".into(),
                usage: Usage {
                    input_tokens: 10,
                    output_tokens: 10,
                },
                model: request.model.clone(),
                latency_ms: 1,
                raw: serde_json::json!({}),
            })
        }
    }

    fn service(daily: &str) -> (RelayService, tempfile::TempDir) {
        let temporary = tempdir().unwrap();
        let paths = AppPaths::new(temporary.path().join("relay"));
        paths.initialize().unwrap();
        let config = Config {
            caps: CapsConfig {
                daily_usd: daily.into(),
                monthly_usd: daily.into(),
                timezone: "UTC".into(),
            },
            models: BTreeMap::from([("default".into(), "test:model".into())]),
            providers: BTreeMap::from([(
                "test".into(),
                ProviderConfig {
                    api_key_env: "TEST_API_KEY".into(),
                },
            )]),
        };
        let price = PriceEntry {
            input_per_mtok: Decimal::from_str("1").unwrap(),
            output_per_mtok: Decimal::from_str("1").unwrap(),
            max_context: 1000,
            api_model: "model".into(),
            verified: true,
            source_url: Some("https://example.test".into()),
            verified_at: Some("2026-07-18".into()),
        };
        let prices =
            PriceCatalog::from_entries(BTreeMap::from([("test:model".into(), price)])).unwrap();
        let ledger = Ledger::open(paths.ledger.clone()).unwrap();
        (
            RelayService {
                paths,
                config,
                prices,
                ledger,
            },
            temporary,
        )
    }

    fn request() -> RelayRequest {
        RelayRequest {
            messages: vec![Message::user("test")],
            model: "model".into(),
            max_output_tokens: Some(10),
            stream: false,
            metadata: Default::default(),
        }
    }

    #[tokio::test]
    async fn at_cap_refusal_makes_zero_provider_requests() {
        let (service, _temporary) = service("0.000020");
        let first = Arc::new(FakeAdapter {
            calls: AtomicUsize::new(0),
        });
        service
            .execute_with_adapter(
                "test:model".into(),
                "test".into(),
                request(),
                first.clone(),
                Utc.with_ymd_and_hms(2026, 7, 18, 12, 0, 0).unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(first.calls.load(Ordering::SeqCst), 1);

        let refused = Arc::new(FakeAdapter {
            calls: AtomicUsize::new(0),
        });
        let result = service
            .execute_with_adapter(
                "test:model".into(),
                "test".into(),
                request(),
                refused.clone(),
                Utc.with_ymd_and_hms(2026, 7, 18, 12, 0, 1).unwrap(),
            )
            .await;
        assert!(matches!(result, Err(RelayError::CapExceeded { .. })));
        assert_eq!(refused.calls.load(Ordering::SeqCst), 0);
    }
}
