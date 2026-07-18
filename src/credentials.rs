// Copyright 2025-present Snab Limited (trading as Korvo)
// SPDX-License-Identifier: Apache-2.0

use std::sync::Arc;

use crate::error::{RelayError, Result};

const SERVICE_NAME: &str = "com.korvo.relay.api-key";

pub trait CredentialStore: Send + Sync {
    fn get(&self, provider: &str) -> Result<Option<String>>;
    fn exists(&self, provider: &str) -> Result<bool>;
    fn set(&self, provider: &str, secret: &str) -> Result<()>;
    fn delete(&self, provider: &str) -> Result<bool>;
}

pub type SharedCredentialStore = Arc<dyn CredentialStore>;

#[derive(Clone, Default)]
pub struct NativeCredentialStore;

impl NativeCredentialStore {
    pub fn shared() -> SharedCredentialStore {
        Arc::new(Self)
    }

    fn entry(provider: &str) -> Result<keyring::Entry> {
        validate_provider(provider)?;
        keyring::Entry::new(SERVICE_NAME, provider)
            .map_err(|error| RelayError::CredentialStore(error.to_string()))
    }
}

impl CredentialStore for NativeCredentialStore {
    fn get(&self, provider: &str) -> Result<Option<String>> {
        match Self::entry(provider)?.get_password() {
            Ok(secret) if secret.trim().is_empty() => Ok(None),
            Ok(secret) => Ok(Some(secret)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(RelayError::CredentialStore(error.to_string())),
        }
    }

    fn exists(&self, provider: &str) -> Result<bool> {
        match Self::entry(provider)?.get_attributes() {
            Ok(_) => Ok(true),
            Err(keyring::Error::NoEntry) => Ok(false),
            Err(error) => Err(RelayError::CredentialStore(error.to_string())),
        }
    }

    fn set(&self, provider: &str, secret: &str) -> Result<()> {
        validate_secret(secret)?;
        Self::entry(provider)?
            .set_password(secret)
            .map_err(|error| RelayError::CredentialStore(error.to_string()))
    }

    fn delete(&self, provider: &str) -> Result<bool> {
        match Self::entry(provider)?.delete_credential() {
            Ok(()) => Ok(true),
            Err(keyring::Error::NoEntry) => Ok(false),
            Err(error) => Err(RelayError::CredentialStore(error.to_string())),
        }
    }
}

pub fn validate_provider(provider: &str) -> Result<()> {
    if provider.is_empty()
        || provider.len() > 64
        || !provider
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(RelayError::Config(format!(
            "invalid provider name '{provider}'"
        )));
    }
    Ok(())
}

pub fn validate_secret(secret: &str) -> Result<()> {
    if secret.trim().is_empty() {
        return Err(RelayError::Config("API key must not be empty".into()));
    }
    if secret.contains(['\n', '\r', '\0']) {
        return Err(RelayError::Config(
            "API key must be a single non-empty line".into(),
        ));
    }
    if secret.len() > 16 * 1024 {
        return Err(RelayError::Config("API key is unexpectedly large".into()));
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::{collections::HashMap, sync::Mutex};

    use super::*;

    #[derive(Default)]
    pub struct MemoryCredentialStore {
        secrets: Mutex<HashMap<String, String>>,
    }

    impl CredentialStore for MemoryCredentialStore {
        fn get(&self, provider: &str) -> Result<Option<String>> {
            validate_provider(provider)?;
            Ok(self.secrets.lock().unwrap().get(provider).cloned())
        }

        fn exists(&self, provider: &str) -> Result<bool> {
            validate_provider(provider)?;
            Ok(self.secrets.lock().unwrap().contains_key(provider))
        }

        fn set(&self, provider: &str, secret: &str) -> Result<()> {
            validate_provider(provider)?;
            validate_secret(secret)?;
            self.secrets
                .lock()
                .unwrap()
                .insert(provider.to_owned(), secret.to_owned());
            Ok(())
        }

        fn delete(&self, provider: &str) -> Result<bool> {
            validate_provider(provider)?;
            Ok(self.secrets.lock().unwrap().remove(provider).is_some())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{test_support::MemoryCredentialStore, *};

    #[test]
    fn memory_store_round_trip_does_not_touch_native_vault() {
        let store = MemoryCredentialStore::default();
        assert_eq!(store.get("openai").unwrap(), None);
        store.set("openai", "test-secret").unwrap();
        assert!(store.exists("openai").unwrap());
        assert_eq!(store.get("openai").unwrap().as_deref(), Some("test-secret"));
        assert!(store.delete("openai").unwrap());
        assert!(!store.delete("openai").unwrap());
    }

    #[test]
    fn rejects_multiline_and_invalid_provider_values() {
        assert!(validate_secret("\n").is_err());
        assert!(validate_secret("secret\nsecond-line").is_err());
        assert!(validate_provider("OpenAI").is_err());
        assert!(validate_provider("../openai").is_err());
    }
}
