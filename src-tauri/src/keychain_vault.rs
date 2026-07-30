//! Capability-scoped credentials held by the macOS Keychain.
//!
//! This module is intentionally usable only by the signed desktop process.
//! It has no Tauri command surface: callers must make an explicit desktop-side
//! decision before reading a capability.  In particular, neither the SQLite
//! store nor the local socket/CLI should receive a value returned from here.

use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};

/// The Keychain service name owned by the Review Queue desktop application.
pub const KEYCHAIN_SERVICE: &str = "com.reviewqueue.desktop";

/// A narrowly scoped Keychain account.  A token for one capability must never
/// be used as credentials for another capability.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    CopilotApp,
    PrRead,
    PrPublish,
    DevicePending,
    CopilotCliOptOut,
}

impl Capability {
    pub const ALL: [Self; 5] = [
        Self::CopilotApp,
        Self::PrRead,
        Self::PrPublish,
        Self::DevicePending,
        Self::CopilotCliOptOut,
    ];

    pub const fn account(self) -> &'static str {
        match self {
            Self::CopilotApp => "copilot_app",
            Self::PrRead => "pr_read",
            Self::PrPublish => "pr_publish",
            Self::DevicePending => "device_pending",
            Self::CopilotCliOptOut => "copilot_cli_opt_out",
        }
    }
}

/// A deliberately redacted error.  Do not attach storage values or platform
/// errors because either could be surfaced in desktop logs by a caller.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VaultError {
    Unavailable,
    InvalidInput,
    StorageFailure,
}

impl fmt::Display for VaultError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Unavailable => "the credential vault is unavailable on this platform",
            Self::InvalidInput => "credential vault input is invalid",
            Self::StorageFailure => "the credential vault operation failed",
        })
    }
}

impl std::error::Error for VaultError {}

/// Minimal storage boundary, kept small so all recovery behavior can be unit
/// tested without touching a real user's Keychain.
pub trait SecretBackend {
    fn set(&self, account: &str, value: &str) -> Result<(), VaultError>;
    fn get(&self, account: &str) -> Result<Option<String>, VaultError>;
    fn delete(&self, account: &str) -> Result<(), VaultError>;
}

/// Pending OAuth device-flow state. The device code is secret and must not be
/// copied to SQLite, diagnostics, the browser, or logs. The user code and
/// verification URL are intentionally public UI fields.
#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DevicePendingState {
    pub public_client_id: String,
    pub target: Capability,
    #[serde(default)]
    pub expected_account_label: Option<String>,
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_at_unix_seconds: i64,
    pub poll_interval_seconds: u64,
}

impl fmt::Debug for DevicePendingState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Device and user codes are credentials. Keep Debug safe because it is
        // commonly included in error reports and structured application logs.
        f.debug_struct("DevicePendingState")
            .field("public_client_id", &self.public_client_id)
            .field("target", &self.target)
            .field("expected_account_label", &self.expected_account_label)
            .field("device_code", &"[REDACTED]")
            .field("user_code", &"[REDACTED]")
            .field("verification_uri", &self.verification_uri)
            .field("expires_at_unix_seconds", &self.expires_at_unix_seconds)
            .field("poll_interval_seconds", &self.poll_interval_seconds)
            .finish()
    }
}

/// An app-owned OAuth credential and its public display metadata. The whole
/// record is stored in the target capability's single Keychain account.
#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppCredentialRecord {
    pub access_token: String,
    #[serde(default)]
    pub account_label: Option<String>,
    #[serde(default)]
    pub scopes: Vec<String>,
    #[serde(default)]
    pub expires_at_unix_seconds: Option<i64>,
}

impl fmt::Debug for AppCredentialRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AppCredentialRecord")
            .field("access_token", &"[REDACTED]")
            .field("account_label", &self.account_label)
            .field("scopes", &self.scopes)
            .field("expires_at_unix_seconds", &self.expires_at_unix_seconds)
            .finish()
    }
}

impl DevicePendingState {
    fn is_valid(&self) -> bool {
        !self.public_client_id.trim().is_empty()
            && matches!(
                self.target,
                Capability::CopilotApp | Capability::PrRead | Capability::PrPublish
            )
            && self
                .expected_account_label
                .as_deref()
                .is_none_or(|label| !label.trim().is_empty() && label.len() <= 128)
            && !self.device_code.is_empty()
            && !self.user_code.is_empty()
            && !self.verification_uri.trim().is_empty()
            && self.expires_at_unix_seconds > 0
            && self.poll_interval_seconds > 0
    }
}

/// Capability-aware facade over a secure credential store.
pub struct CredentialVault<B> {
    backend: B,
}

impl<B> CredentialVault<B>
where
    B: SecretBackend,
{
    pub fn new(backend: B) -> Self {
        Self { backend }
    }

    pub fn set(&self, capability: Capability, value: &str) -> Result<(), VaultError> {
        if value.is_empty() {
            return Err(VaultError::InvalidInput);
        }
        self.backend.set(capability.account(), value)
    }

    /// Returns `None` for an absent or malformed entry.  Malformed records are
    /// removed immediately so a bad Keychain item cannot repeatedly poison
    /// subsequent sign-in attempts.
    pub fn get(&self, capability: Capability) -> Result<Option<String>, VaultError> {
        match self.backend.get(capability.account())? {
            Some(value) if !value.is_empty() => Ok(Some(value)),
            Some(_) => {
                self.backend.delete(capability.account())?;
                Ok(None)
            }
            None => Ok(None),
        }
    }

    pub fn delete(&self, capability: Capability) -> Result<(), VaultError> {
        self.backend.delete(capability.account())
    }

    pub fn set_device_pending(&self, state: &DevicePendingState) -> Result<(), VaultError> {
        if !state.is_valid() {
            return Err(VaultError::InvalidInput);
        }
        let encoded = serde_json::to_string(state).map_err(|_| VaultError::InvalidInput)?;
        self.backend
            .set(Capability::DevicePending.account(), &encoded)
    }

    pub fn set_app_credential(
        &self,
        capability: Capability,
        record: &AppCredentialRecord,
    ) -> Result<(), VaultError> {
        if !matches!(
            capability,
            Capability::CopilotApp | Capability::PrRead | Capability::PrPublish
        ) || record.access_token.is_empty()
        {
            return Err(VaultError::InvalidInput);
        }
        let encoded = serde_json::to_string(record).map_err(|_| VaultError::InvalidInput)?;
        self.backend.set(capability.account(), &encoded)
    }

    /// Loads a structured credential without exposing it over IPC. Legacy
    /// opaque values remain readable so upgrades do not disconnect users.
    pub fn get_app_credential(
        &self,
        capability: Capability,
    ) -> Result<Option<AppCredentialRecord>, VaultError> {
        if !matches!(
            capability,
            Capability::CopilotApp | Capability::PrRead | Capability::PrPublish
        ) {
            return Err(VaultError::InvalidInput);
        }
        let Some(encoded) = self.get(capability)? else {
            return Ok(None);
        };
        if !encoded.trim_start().starts_with('{') {
            return Ok(Some(AppCredentialRecord {
                access_token: encoded,
                account_label: None,
                scopes: Vec::new(),
                expires_at_unix_seconds: None,
            }));
        }
        match serde_json::from_str::<AppCredentialRecord>(&encoded) {
            Ok(record) if !record.access_token.is_empty() => Ok(Some(record)),
            _ => {
                self.delete(capability)?;
                Ok(None)
            }
        }
    }

    /// Recover from a partial/old/corrupt device record by deleting just that
    /// item.  Other app capabilities remain available.
    pub fn get_device_pending(&self) -> Result<Option<DevicePendingState>, VaultError> {
        let Some(encoded) = self.get(Capability::DevicePending)? else {
            return Ok(None);
        };
        match serde_json::from_str::<DevicePendingState>(&encoded) {
            Ok(state) if state.is_valid() => Ok(Some(state)),
            _ => {
                self.delete(Capability::DevicePending)?;
                Ok(None)
            }
        }
    }

    /// If the public OAuth client identifier changes, invalidate only records
    /// owned by this application.  This prevents tokens minted for a previous
    /// public client from being replayed under the new client identity.
    ///
    /// The marker itself is intentionally in the Keychain rather than SQLite.
    /// Returns whether invalidation was performed.
    pub fn invalidate_for_public_client_id(
        &self,
        public_client_id: &str,
    ) -> Result<bool, VaultError> {
        if public_client_id.trim().is_empty() {
            return Err(VaultError::InvalidInput);
        }
        let marker = self.backend.get(PUBLIC_CLIENT_ID_ACCOUNT)?;
        if marker.as_deref() == Some(public_client_id) {
            return Ok(false);
        }
        for capability in Capability::ALL {
            self.backend.delete(capability.account())?;
        }
        self.backend
            .set(PUBLIC_CLIENT_ID_ACCOUNT, public_client_id)?;
        Ok(true)
    }

    /// Returns the public OAuth client identifier marker. This is public
    /// configuration, not a credential.
    pub fn public_client_id(&self) -> Result<Option<String>, VaultError> {
        match self.backend.get(PUBLIC_CLIENT_ID_ACCOUNT)? {
            Some(value) if !value.trim().is_empty() => Ok(Some(value)),
            Some(_) => {
                self.backend.delete(PUBLIC_CLIENT_ID_ACCOUNT)?;
                Ok(None)
            }
            None => Ok(None),
        }
    }

    /// Records the initial bundled client identifier without invalidating
    /// credentials. Replacing an existing marker must go through
    /// `invalidate_for_public_client_id` and its caller's confirmation UI.
    pub fn initialize_public_client_id(&self, public_client_id: &str) -> Result<bool, VaultError> {
        if public_client_id.trim().is_empty() {
            return Err(VaultError::InvalidInput);
        }
        if self.public_client_id()?.is_some() {
            return Ok(false);
        }
        self.backend
            .set(PUBLIC_CLIENT_ID_ACCOUNT, public_client_id)?;
        Ok(true)
    }

    pub fn into_backend(self) -> B {
        self.backend
    }
}

const PUBLIC_CLIENT_ID_ACCOUNT: &str = "public_client_id";

/// Security.framework-backed Keychain implementation used by the macOS desktop
/// app. `keyring`'s `apple-native` feature selects its Security.framework
/// backend on macOS; this is deliberately not a plaintext fallback.
pub struct MacOsKeychainBackend {
    service: String,
}

impl MacOsKeychainBackend {
    pub fn new() -> Self {
        Self::with_service(KEYCHAIN_SERVICE)
    }

    /// Intended for isolated tests only. Production callers must use `new`.
    pub fn with_service(service: impl Into<String>) -> Self {
        Self {
            service: service.into(),
        }
    }
}

impl Default for MacOsKeychainBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl SecretBackend for MacOsKeychainBackend {
    fn set(&self, account: &str, value: &str) -> Result<(), VaultError> {
        let entry =
            keyring::Entry::new(&self.service, account).map_err(|_| VaultError::StorageFailure)?;
        entry
            .set_password(value)
            .map_err(|_| VaultError::StorageFailure)
    }

    fn get(&self, account: &str) -> Result<Option<String>, VaultError> {
        let entry =
            keyring::Entry::new(&self.service, account).map_err(|_| VaultError::StorageFailure)?;
        match entry.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(_) => Err(VaultError::StorageFailure),
        }
    }

    fn delete(&self, account: &str) -> Result<(), VaultError> {
        let entry =
            keyring::Entry::new(&self.service, account).map_err(|_| VaultError::StorageFailure)?;
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(_) => Err(VaultError::StorageFailure),
        }
    }
}

impl FromStr for Capability {
    type Err = VaultError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "copilot_app" => Ok(Self::CopilotApp),
            "pr_read" => Ok(Self::PrRead),
            "pr_publish" => Ok(Self::PrPublish),
            "device_pending" => Ok(Self::DevicePending),
            "copilot_cli_opt_out" => Ok(Self::CopilotCliOptOut),
            _ => Err(VaultError::InvalidInput),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, collections::BTreeMap};

    #[derive(Default)]
    struct InMemoryBackend(RefCell<BTreeMap<String, String>>);

    impl SecretBackend for InMemoryBackend {
        fn set(&self, account: &str, value: &str) -> Result<(), VaultError> {
            self.0
                .borrow_mut()
                .insert(account.to_owned(), value.to_owned());
            Ok(())
        }

        fn get(&self, account: &str) -> Result<Option<String>, VaultError> {
            Ok(self.0.borrow().get(account).cloned())
        }

        fn delete(&self, account: &str) -> Result<(), VaultError> {
            self.0.borrow_mut().remove(account);
            Ok(())
        }
    }

    fn pending(client_id: &str) -> DevicePendingState {
        DevicePendingState {
            public_client_id: client_id.into(),
            target: Capability::CopilotApp,
            expected_account_label: None,
            device_code: "device-code".into(),
            user_code: "user-code".into(),
            verification_uri: "https://example.invalid/activate".into(),
            expires_at_unix_seconds: 1_900_000_000,
            poll_interval_seconds: 5,
        }
    }

    #[test]
    fn credentials_are_scoped_to_named_capability_accounts() {
        let vault = CredentialVault::new(InMemoryBackend::default());
        vault.set(Capability::CopilotApp, "opaque-value").unwrap();
        assert!(vault.get(Capability::PrRead).unwrap().is_none());
        assert!(vault.get(Capability::CopilotApp).unwrap().is_some());
        vault.delete(Capability::CopilotApp).unwrap();
        assert!(vault.get(Capability::CopilotApp).unwrap().is_none());
    }

    #[test]
    fn malformed_device_state_is_deleted_without_touching_other_capabilities() {
        let backend = InMemoryBackend::default();
        backend
            .set(Capability::DevicePending.account(), "not-json")
            .unwrap();
        backend
            .set(Capability::PrRead.account(), "opaque-value")
            .unwrap();
        let vault = CredentialVault::new(backend);
        assert!(vault.get_device_pending().unwrap().is_none());
        assert!(vault.get(Capability::PrRead).unwrap().is_some());
        assert!(vault.get(Capability::DevicePending).unwrap().is_none());
    }

    #[test]
    fn changed_public_client_id_invalidates_app_owned_records() {
        let vault = CredentialVault::new(InMemoryBackend::default());
        assert!(vault.invalidate_for_public_client_id("client-a").unwrap());
        vault.set(Capability::CopilotApp, "opaque-value").unwrap();
        vault.set_device_pending(&pending("client-a")).unwrap();
        assert!(!vault.invalidate_for_public_client_id("client-a").unwrap());
        assert!(vault.get(Capability::CopilotApp).unwrap().is_some());
        assert!(vault.invalidate_for_public_client_id("client-b").unwrap());
        assert!(vault.get(Capability::CopilotApp).unwrap().is_none());
        assert!(vault.get_device_pending().unwrap().is_none());
    }

    #[test]
    fn initial_public_client_marker_does_not_clear_existing_records() {
        let vault = CredentialVault::new(InMemoryBackend::default());
        vault.set(Capability::PrRead, "opaque-value").unwrap();
        assert!(vault.initialize_public_client_id("client-a").unwrap());
        assert_eq!(
            vault.public_client_id().unwrap().as_deref(),
            Some("client-a")
        );
        assert!(vault.get(Capability::PrRead).unwrap().is_some());
        assert!(!vault.initialize_public_client_id("client-b").unwrap());
        assert_eq!(
            vault.public_client_id().unwrap().as_deref(),
            Some("client-a")
        );
    }

    #[test]
    fn capability_names_are_stable() {
        assert_eq!(Capability::CopilotApp.account(), "copilot_app");
        assert_eq!(Capability::PrRead.account(), "pr_read");
        assert_eq!(Capability::PrPublish.account(), "pr_publish");
        assert_eq!(Capability::DevicePending.account(), "device_pending");
        assert_eq!(
            Capability::CopilotCliOptOut.account(),
            "copilot_cli_opt_out"
        );
    }

    #[test]
    fn device_pending_debug_is_redacted() {
        let state = pending("client-a");
        let rendered = format!("{state:?}");
        assert!(!rendered.contains("device-code"));
        assert!(!rendered.contains("user-code"));
        assert!(rendered.contains("[REDACTED]"));
    }

    #[test]
    fn app_credential_record_is_scoped_and_debug_redacted() {
        let vault = CredentialVault::new(InMemoryBackend::default());
        let record = AppCredentialRecord {
            access_token: "github-secret-token".into(),
            account_label: Some("octocat".into()),
            scopes: vec!["repo".into()],
            expires_at_unix_seconds: None,
        };
        vault
            .set_app_credential(Capability::PrRead, &record)
            .unwrap();
        assert_eq!(
            vault.get_app_credential(Capability::PrRead).unwrap(),
            Some(record.clone())
        );
        assert!(
            vault
                .get_app_credential(Capability::PrPublish)
                .unwrap()
                .is_none()
        );
        assert!(!format!("{record:?}").contains("github-secret-token"));
    }

    #[test]
    #[ignore = "uses the macOS login Keychain; run explicitly on a signed desktop host"]
    fn macos_keychain_round_trip_uses_disposable_service() {
        let service = format!("com.reviewqueue.desktop.test.{}", std::process::id());
        let vault = CredentialVault::new(MacOsKeychainBackend::with_service(service));
        vault.delete(Capability::PrRead).unwrap();
        vault.set(Capability::PrRead, "disposable-value").unwrap();
        assert!(vault.get(Capability::PrRead).unwrap().is_some());
        vault.delete(Capability::PrRead).unwrap();
        assert!(vault.get(Capability::PrRead).unwrap().is_none());
    }
}
