use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

use crate::{
    error::{RelayError, Result},
    pricing::usd_to_microusd,
};

const DEFAULT_CONFIG: &str = r#"[caps]
daily_usd = "5.00"
monthly_usd = "50.00"
timezone = "UTC"

[models]
default = "openai:gpt-4o-mini"
think = "anthropic:claude-sonnet"

[providers.openai]
api_key_env = "OPENAI_API_KEY"

[providers.anthropic]
api_key_env = "ANTHROPIC_API_KEY"
"#;

const DEFAULT_PRICES: &str = include_str!("../prices.json");

#[derive(Clone, Debug)]
pub struct AppPaths {
    pub home: PathBuf,
    pub config: PathBuf,
    pub prices: PathBuf,
    pub ledger: PathBuf,
}

impl AppPaths {
    pub fn discover() -> Result<Self> {
        let home = dirs::home_dir()
            .ok_or_else(|| {
                RelayError::Config("could not determine the user home directory".into())
            })?
            .join(".relay");
        Ok(Self::new(home))
    }

    pub fn new(home: PathBuf) -> Self {
        Self {
            config: home.join("relay.toml"),
            prices: home.join("prices.json"),
            ledger: home.join("ledger.db"),
            home,
        }
    }

    pub fn initialize(&self) -> Result<()> {
        fs::create_dir_all(&self.home)?;
        set_private_dir_permissions(&self.home)?;
        create_private_file_if_missing(&self.config, DEFAULT_CONFIG)?;
        create_private_file_if_missing(&self.prices, DEFAULT_PRICES)?;
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Config {
    pub caps: CapsConfig,
    pub models: BTreeMap<String, String>,
    pub providers: BTreeMap<String, ProviderConfig>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CapsConfig {
    #[serde(deserialize_with = "deserialize_amount")]
    pub daily_usd: String,
    #[serde(deserialize_with = "deserialize_amount")]
    pub monthly_usd: String,
    pub timezone: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ProviderConfig {
    pub api_key_env: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CapLimits {
    pub daily_microusd: i64,
    pub monthly_microusd: i64,
    pub timezone: Tz,
}

impl Config {
    pub fn load_or_create(paths: &AppPaths) -> Result<Self> {
        paths.initialize()?;
        let config: Self = toml::from_str(&fs::read_to_string(&paths.config)?)?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<()> {
        self.cap_limits()?;
        if self.models.get("default").is_none_or(String::is_empty) {
            return Err(RelayError::Config(
                "[models].default must name a model".into(),
            ));
        }
        for (provider, config) in &self.providers {
            if config.api_key_env.trim().is_empty() {
                return Err(RelayError::Config(format!(
                    "providers.{provider}.api_key_env must not be empty"
                )));
            }
        }
        Ok(())
    }

    pub fn cap_limits(&self) -> Result<CapLimits> {
        let timezone = self.caps.timezone.parse::<Tz>().map_err(|_| {
            RelayError::Config(format!(
                "unknown IANA timezone '{}' in [caps]",
                self.caps.timezone
            ))
        })?;
        Ok(CapLimits {
            daily_microusd: usd_to_microusd(&self.caps.daily_usd)?,
            monthly_microusd: usd_to_microusd(&self.caps.monthly_usd)?,
            timezone,
        })
    }

    pub fn resolve_model(&self, requested: Option<&str>, think: bool) -> Result<String> {
        let name = if think {
            "think"
        } else {
            requested.unwrap_or("default")
        };
        if name.contains(':') {
            return Ok(name.to_owned());
        }
        self.models
            .get(name)
            .cloned()
            .ok_or_else(|| RelayError::UnknownModel(name.to_owned()))
    }

    pub fn credential(&self, provider: &str) -> Result<String> {
        let variable = self
            .providers
            .get(provider)
            .ok_or_else(|| RelayError::UnsupportedProvider(provider.to_owned()))?
            .api_key_env
            .trim();
        std::env::var(variable)
            .ok()
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| RelayError::MissingCredential(variable.to_owned()))
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        self.validate()?;
        let temporary = path.with_extension("toml.tmp");
        fs::write(&temporary, toml::to_string_pretty(self)?)?;
        set_private_file_permissions(&temporary)?;
        fs::rename(temporary, path)?;
        Ok(())
    }
}

fn create_private_file_if_missing(path: &Path, contents: &str) -> Result<()> {
    if path.exists() {
        return Ok(());
    }
    let temporary = path.with_extension("tmp");
    match fs::write(&temporary, contents) {
        Ok(()) => {
            set_private_file_permissions(&temporary)?;
            match fs::rename(&temporary, path) {
                Ok(()) => Ok(()),
                Err(_error) if path.exists() => {
                    let _ = fs::remove_file(temporary);
                    Ok(())
                }
                Err(error) => Err(error.into()),
            }
        }
        Err(error) => Err(error.into()),
    }
}

fn deserialize_amount<'de, D>(deserializer: D) -> std::result::Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Amount {
        String(String),
        Integer(i64),
        Float(f64),
    }

    Ok(match Amount::deserialize(deserializer)? {
        Amount::String(value) => value,
        Amount::Integer(value) => value.to_string(),
        Amount::Float(value) => value.to_string(),
    })
}

#[cfg(unix)]
fn set_private_dir_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_private_dir_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn set_private_file_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_private_file_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn first_run_creates_local_files_and_loads_defaults() {
        let temporary = tempdir().unwrap();
        let paths = AppPaths::new(temporary.path().join("relay"));
        let config = Config::load_or_create(&paths).unwrap();
        assert_eq!(config.cap_limits().unwrap().daily_microusd, 5_000_000);
        assert!(paths.prices.exists());
        assert!(paths.ledger.parent().unwrap().exists());
    }

    #[test]
    fn aliases_resolve_without_changing_canonical_models() {
        let config: Config = toml::from_str(DEFAULT_CONFIG).unwrap();
        assert_eq!(
            config.resolve_model(Some("think"), false).unwrap(),
            "anthropic:claude-sonnet"
        );
        assert_eq!(
            config.resolve_model(Some("openai:custom"), false).unwrap(),
            "openai:custom"
        );
    }

    #[test]
    fn numeric_toml_caps_are_accepted() {
        let config = DEFAULT_CONFIG
            .replace("\"5.00\"", "5.00")
            .replace("\"50.00\"", "50.00");
        let config: Config = toml::from_str(&config).unwrap();
        assert_eq!(config.cap_limits().unwrap().monthly_microusd, 50_000_000);
    }
}
