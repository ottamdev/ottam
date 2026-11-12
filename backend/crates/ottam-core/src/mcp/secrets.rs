//! Where MCP server secrets live: env values and headers marked secret, and
//! later OAuth tokens. On macOS that is the login Keychain; elsewhere, and
//! when a test build is told `OTTAM_SECRET_STORE=memory`, a process-local map
//! that forgets everything at exit. Calls block, so callers run them off the
//! runtime.

use crate::core_error::CoreError;
#[cfg(any(test_seams, not(target_os = "macos")))]
use std::collections::HashMap;
use std::sync::Arc;
#[cfg(any(test_seams, not(target_os = "macos")))]
use std::sync::Mutex;

pub(crate) trait SecretStore: Send + Sync + 'static {
    fn get(&self, account: &str) -> Result<Option<String>, CoreError>;
    fn set(&self, account: &str, value: &str) -> Result<(), CoreError>;
    /// Succeeds when nothing is stored under `account`.
    fn delete(&self, account: &str) -> Result<(), CoreError>;
}

/// The Keychain on macOS unless a test build's environment asks for memory.
pub(crate) fn default_secret_store() -> Arc<dyn SecretStore> {
    #[cfg(test_seams)]
    if std::env::var("OTTAM_SECRET_STORE").as_deref() == Ok("memory") {
        return Arc::new(MemorySecretStore::default());
    }
    platform_secret_store()
}

#[cfg(target_os = "macos")]
fn platform_secret_store() -> Arc<dyn SecretStore> {
    Arc::new(keychain::KeychainSecretStore)
}

#[cfg(not(target_os = "macos"))]
fn platform_secret_store() -> Arc<dyn SecretStore> {
    tracing::info!("no system keychain on this platform; MCP secrets last until core exits");
    Arc::new(MemorySecretStore::default())
}

#[cfg(any(test_seams, not(target_os = "macos")))]
#[derive(Default)]
pub(crate) struct MemorySecretStore {
    values: Mutex<HashMap<String, String>>,
}

#[cfg(any(test_seams, not(target_os = "macos")))]
impl MemorySecretStore {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, String>> {
        self.values
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[cfg(any(test_seams, not(target_os = "macos")))]
impl SecretStore for MemorySecretStore {
    fn get(&self, account: &str) -> Result<Option<String>, CoreError> {
        Ok(self.lock().get(account).cloned())
    }

    fn set(&self, account: &str, value: &str) -> Result<(), CoreError> {
        self.lock().insert(account.to_owned(), value.to_owned());
        Ok(())
    }

    fn delete(&self, account: &str) -> Result<(), CoreError> {
        self.lock().remove(account);
        Ok(())
    }
}

#[cfg(target_os = "macos")]
mod keychain {
    use super::SecretStore;
    use crate::core_error::CoreError;
    use security_framework::passwords::{
        delete_generic_password, get_generic_password, set_generic_password,
    };

    const SERVICE: &str = "dev.ottam.Ottam.mcp";
    /// `errSecItemNotFound`.
    const ITEM_NOT_FOUND: i32 = -25300;

    /// Generic passwords under one service, one account per secret.
    pub(super) struct KeychainSecretStore;

    fn unavailable(error: security_framework::base::Error) -> CoreError {
        tracing::warn!(code = error.code(), "Keychain request failed");
        CoreError::new(
            "secretStoreUnavailable",
            format!("The Keychain refused the request ({})", error.code()),
        )
        .recoverable(true)
    }

    impl SecretStore for KeychainSecretStore {
        fn get(&self, account: &str) -> Result<Option<String>, CoreError> {
            match get_generic_password(SERVICE, account) {
                Ok(bytes) => String::from_utf8(bytes).map(Some).map_err(|_| {
                    CoreError::new("secretStoreUnavailable", "a stored secret is not text")
                }),
                Err(error) if error.code() == ITEM_NOT_FOUND => Ok(None),
                Err(error) => Err(unavailable(error)),
            }
        }

        fn set(&self, account: &str, value: &str) -> Result<(), CoreError> {
            set_generic_password(SERVICE, account, value.as_bytes()).map_err(unavailable)
        }

        fn delete(&self, account: &str) -> Result<(), CoreError> {
            match delete_generic_password(SERVICE, account) {
                Ok(()) => Ok(()),
                Err(error) if error.code() == ITEM_NOT_FOUND => Ok(()),
                Err(error) => Err(unavailable(error)),
            }
        }
    }
}
