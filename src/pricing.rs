use std::{collections::BTreeMap, fs, path::Path, str::FromStr};

use rust_decimal::{Decimal, prelude::ToPrimitive};
use serde::{Deserialize, Deserializer, Serialize};

use crate::error::{RelayError, Result};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PriceEntry {
    #[serde(deserialize_with = "deserialize_decimal")]
    pub input_per_mtok: Decimal,
    #[serde(deserialize_with = "deserialize_decimal")]
    pub output_per_mtok: Decimal,
    pub max_context: u64,
    pub api_model: String,
    #[serde(default)]
    pub verified: bool,
    pub source_url: Option<String>,
    pub verified_at: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(transparent)]
pub struct PriceCatalog {
    entries: BTreeMap<String, PriceEntry>,
}

impl PriceCatalog {
    pub fn load(path: &Path) -> Result<Self> {
        let catalog: Self = serde_json::from_str(&fs::read_to_string(path)?)?;
        for (model, entry) in &catalog.entries {
            entry.validate(model)?;
        }
        Ok(catalog)
    }

    pub fn from_entries(entries: BTreeMap<String, PriceEntry>) -> Result<Self> {
        let catalog = Self { entries };
        for (model, entry) in &catalog.entries {
            entry.validate(model)?;
        }
        Ok(catalog)
    }

    pub fn entries(&self) -> impl Iterator<Item = (&str, &PriceEntry)> {
        self.entries
            .iter()
            .map(|(key, value)| (key.as_str(), value))
    }

    pub fn get(&self, model: &str) -> Result<&PriceEntry> {
        self.entries
            .get(model)
            .ok_or_else(|| RelayError::UnknownModel(model.to_owned()))
    }

    pub fn get_verified(&self, model: &str) -> Result<&PriceEntry> {
        let entry = self.get(model)?;
        if !entry.verified {
            return Err(RelayError::InvalidPrice {
                model: model.to_owned(),
                reason: "price is not marked verified; review its official source before making a paid call"
                    .to_owned(),
            });
        }
        Ok(entry)
    }
}

impl PriceEntry {
    pub fn validate(&self, model: &str) -> Result<()> {
        let fail = |reason: &str| RelayError::InvalidPrice {
            model: model.to_owned(),
            reason: reason.to_owned(),
        };

        let mut parts = model.split(':');
        if parts.next().is_none_or(str::is_empty)
            || parts.next().is_none_or(str::is_empty)
            || parts.next().is_some()
        {
            return Err(fail("key must have the form provider:model"));
        }
        if self.input_per_mtok.is_sign_negative() || self.output_per_mtok.is_sign_negative() {
            return Err(fail("prices must be non-negative"));
        }
        if self.max_context == 0 {
            return Err(fail("max_context must be positive"));
        }
        if self.api_model.trim().is_empty() {
            return Err(fail("api_model must not be empty"));
        }
        if self.verified && (self.source_url.is_none() || self.verified_at.is_none()) {
            return Err(fail(
                "verified entries require source_url and verified_at provenance",
            ));
        }
        Ok(())
    }

    pub fn cost_microusd(&self, input_tokens: u64, output_tokens: u64) -> Result<i64> {
        let input = Decimal::from(input_tokens)
            .checked_mul(self.input_per_mtok)
            .ok_or_else(|| RelayError::AmountOutOfRange("input token cost overflow".into()))?;
        let output = Decimal::from(output_tokens)
            .checked_mul(self.output_per_mtok)
            .ok_or_else(|| RelayError::AmountOutOfRange("output token cost overflow".into()))?;
        input
            .checked_add(output)
            .ok_or_else(|| RelayError::AmountOutOfRange("total token cost overflow".into()))?
            .ceil()
            .to_i64()
            .ok_or_else(|| RelayError::AmountOutOfRange("cost does not fit in i64".into()))
    }
}

pub fn usd_to_microusd(amount: &str) -> Result<i64> {
    let decimal = Decimal::from_str(amount.trim())
        .map_err(|error| RelayError::Config(format!("invalid USD amount '{amount}': {error}")))?;
    if decimal.is_sign_negative() {
        return Err(RelayError::Config("caps must not be negative".into()));
    }
    decimal
        .checked_mul(Decimal::from(1_000_000_u64))
        .ok_or_else(|| RelayError::AmountOutOfRange(amount.to_owned()))?
        .floor()
        .to_i64()
        .ok_or_else(|| RelayError::AmountOutOfRange(amount.to_owned()))
}

pub fn format_usd(microusd: i64) -> String {
    let amount = Decimal::from(microusd) / Decimal::from(1_000_000_u64);
    format!("${amount:.6}")
}

fn deserialize_decimal<'de, D>(deserializer: D) -> std::result::Result<Decimal, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum DecimalInput {
        String(String),
        Number(serde_json::Number),
    }

    let input = DecimalInput::deserialize(deserializer)?;
    let value = match input {
        DecimalInput::String(value) => value,
        DecimalInput::Number(value) => value.to_string(),
    };
    Decimal::from_str(&value).map_err(serde::de::Error::custom)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn price(input: &str, output: &str) -> PriceEntry {
        PriceEntry {
            input_per_mtok: Decimal::from_str(input).unwrap(),
            output_per_mtok: Decimal::from_str(output).unwrap(),
            max_context: 128_000,
            api_model: "test-model".into(),
            verified: true,
            source_url: Some("https://example.test/pricing".into()),
            verified_at: Some("2026-07-18".into()),
        }
    }

    #[test]
    fn cost_is_stored_as_conservatively_rounded_microusd() {
        let entry = price("0.15", "0.60");
        assert_eq!(entry.cost_microusd(1, 0).unwrap(), 1);
        assert_eq!(entry.cost_microusd(1_000_000, 1_000_000).unwrap(), 750_000);
    }

    #[test]
    fn usd_caps_floor_sub_microusd_values() {
        assert_eq!(usd_to_microusd("5.00").unwrap(), 5_000_000);
        assert_eq!(usd_to_microusd("0.0000019").unwrap(), 1);
    }

    #[test]
    fn negative_prices_are_rejected() {
        let entry = price("-0.01", "1");
        assert!(entry.validate("test:model").is_err());
    }
}
