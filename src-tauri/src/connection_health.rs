//! Public connection health and first-launch OAuth state.
//!
//! This is the only desktop boundary that probes the existing `copilot` CLI.
//! The probe is deliberately read-only and never invokes login, logout,
//! credential refresh, or credential export commands. App-owned credentials
//! remain capability-scoped in the macOS Keychain.

use crate::keychain_vault::{
    AppCredentialRecord, Capability, CredentialVault, DevicePendingState, MacOsKeychainBackend,
    SecretBackend, VaultError,
};
use serde::{Deserialize, Serialize};
use std::{
    fmt,
    process::{Command, Stdio},
    str::FromStr,
    time::{SystemTime, UNIX_EPOCH},
};

/// Public OAuth application identifier for Review Queue. This is not a
/// credential; users may replace it from Settings through the confirmed
/// client-ID change flow.
const BUNDLED_PUBLIC_CLIENT_ID: Option<&str> = Some("Ov23li9NHgxO6prQz5f7");
const KEYCHAIN_RECOVERY: &str = "Open Keychain Access, select the login keychain, unlock it, return to Review Queue, and choose Retry connection.";

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionSource {
    ExistingCopilotCli,
    AppOwnedOauth,
    None,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionState {
    Connected,
    NotConnected,
    Unavailable,
}

#[derive(Clone, Debug, Serialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CliSignInStatus {
    pub installed: bool,
    pub signed_in: bool,
    pub account: Option<String>,
    pub validation_is_read_only: bool,
}

#[derive(Clone, Debug, Serialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityStatus {
    pub capability: String,
    pub state: ConnectionState,
    pub source: ConnectionSource,
    pub account: Option<String>,
    pub scopes: Vec<String>,
    pub expires_at_unix_seconds: Option<i64>,
    pub optional: bool,
    pub explanation: String,
}

#[derive(Clone, Copy, Debug, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum DeviceFlowPhase {
    Pending,
    SlowDown,
    Connected,
    Expired,
    Denied,
    AccountMismatch,
}

/// Safe for webview IPC. The provider's secret `device_code` is intentionally
/// absent. `Debug` redacts the public code as defense in depth for support
/// logs, even though the code is intentionally displayed in the UI.
#[derive(Clone, Serialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DeviceFlowPublicState {
    pub capability: String,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_at_unix_seconds: i64,
    pub seconds_remaining: u64,
    pub phase: DeviceFlowPhase,
    pub can_cancel: bool,
}

impl fmt::Debug for DeviceFlowPublicState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeviceFlowPublicState")
            .field("capability", &self.capability)
            .field("user_code", &"[REDACTED]")
            .field("verification_uri", &self.verification_uri)
            .field("expires_at_unix_seconds", &self.expires_at_unix_seconds)
            .field("seconds_remaining", &self.seconds_remaining)
            .field("phase", &self.phase)
            .field("can_cancel", &self.can_cancel)
            .finish()
    }
}

#[derive(Clone, Debug, Serialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct KeychainHealth {
    pub available: bool,
    pub service: String,
    pub recovery_instructions: Option<String>,
}

#[derive(Clone, Debug, Serialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionHealth {
    pub cli: CliSignInStatus,
    pub copilot: CapabilityStatus,
    pub pr_read: CapabilityStatus,
    pub pr_publish: CapabilityStatus,
    pub keychain: KeychainHealth,
    pub public_client_id: Option<String>,
    pub pending_device_flow: Option<DeviceFlowPublicState>,
}

#[derive(Clone, Serialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionCommandError {
    pub code: String,
    pub message: String,
    pub data_safety: String,
    pub next_step: String,
}

impl fmt::Debug for ConnectionCommandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConnectionCommandError")
            .field("code", &self.code)
            .field("message", &self.message)
            .field("data_safety", &self.data_safety)
            .field("next_step", &self.next_step)
            .finish()
    }
}

impl From<VaultError> for ConnectionCommandError {
    fn from(_: VaultError) -> Self {
        Self {
            code: "keychain_unavailable".into(),
            message: "Review Queue could not access its credential records in the login Keychain."
                .into(),
            data_safety: "No credential, review snapshot, comment, or SQLite record was changed."
                .into(),
            next_step: KEYCHAIN_RECOVERY.into(),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DisconnectCapabilityRequest {
    pub capability: String,
    pub source: ConnectionSource,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StartDeviceFlowRequest {
    pub capability: String,
    #[serde(default)]
    pub expected_account: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChangePublicClientIdRequest {
    pub public_client_id: String,
    pub confirmed: bool,
}

#[derive(Clone, Debug, Serialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ChangePublicClientIdResult {
    pub changed: bool,
    pub app_owned_records_cleared: bool,
    pub sqlite_preserved: bool,
}

#[derive(Clone, Serialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DeviceFlowPollPublicResult {
    pub capability: String,
    pub phase: DeviceFlowPhase,
    pub account: Option<String>,
    pub seconds_remaining: u64,
    pub retry_after_seconds: Option<u64>,
    pub message: String,
}

impl fmt::Debug for DeviceFlowPollPublicResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeviceFlowPollPublicResult")
            .field("capability", &self.capability)
            .field("phase", &self.phase)
            .field("account", &self.account)
            .field("seconds_remaining", &self.seconds_remaining)
            .field("retry_after_seconds", &self.retry_after_seconds)
            .field("message", &self.message)
            .finish()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CliProbeResult {
    pub installed: bool,
    pub signed_in: bool,
    pub account: Option<String>,
}

pub trait CopilotCliProbe {
    fn validate_read_only(&self) -> CliProbeResult;
}

pub struct SystemCopilotCliProbe;

impl CopilotCliProbe for SystemCopilotCliProbe {
    fn validate_read_only(&self) -> CliProbeResult {
        // Copilot CLI 1.x deliberately has no `auth status` subcommand.  First
        // prove that the executable exists, then inspect only credential
        // metadata. `security find-generic-password` omits the secret unless
        // `-w` is supplied; never add that flag here.
        let output = Command::new("copilot")
            .arg("version")
            .stdin(Stdio::null())
            .output();
        match output {
            Ok(output) => output,
            Err(error) => {
                return CliProbeResult {
                    installed: error.kind() != std::io::ErrorKind::NotFound,
                    signed_in: false,
                    account: None,
                };
            }
        };

        #[cfg(target_os = "macos")]
        if let Ok(metadata) = Command::new("/usr/bin/security")
            .args(["find-generic-password", "-s", "copilot-cli"])
            .stdin(Stdio::null())
            .output()
            && metadata.status.success()
        {
            return CliProbeResult {
                installed: true,
                signed_in: true,
                account: parse_keychain_account(&metadata.stdout)
                    .or_else(|| parse_keychain_account(&metadata.stderr)),
            };
        }

        CliProbeResult {
            installed: true,
            signed_in: false,
            account: None,
        }
    }
}

fn parse_keychain_account(bytes: &[u8]) -> Option<String> {
    let output = String::from_utf8_lossy(bytes);
    output.lines().find_map(|line| {
        let (_, value) = line.split_once("\"acct\"<blob>=\"")?;
        let account = value.strip_suffix('"')?.trim();
        if account.is_empty() {
            return None;
        }
        Some(
            account
                .rsplit_once(':')
                .map(|(_, label)| label)
                .unwrap_or(account)
                .to_owned(),
        )
    })
}

fn sanitize_account_label(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()
        && value.len() <= 128
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '@' | '.' | '-' | '_' | '+')))
    .then(|| value.to_owned())
}

pub trait Clock {
    fn unix_seconds(&self) -> i64;
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn unix_seconds(&self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs() as i64)
            .unwrap_or_default()
    }
}

pub struct IssuedDeviceFlow {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in_seconds: u64,
    pub poll_interval_seconds: u64,
}

pub trait DeviceFlowIssuer {
    fn start(
        &self,
        public_client_id: &str,
        capability: Capability,
    ) -> Result<IssuedDeviceFlow, ConnectionCommandError>;
}

pub struct GitHubDeviceFlowIssuer;

#[derive(Deserialize)]
struct GitHubDeviceCodeResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    expires_in: u64,
    interval: u64,
}

impl DeviceFlowIssuer for GitHubDeviceFlowIssuer {
    fn start(
        &self,
        public_client_id: &str,
        capability: Capability,
    ) -> Result<IssuedDeviceFlow, ConnectionCommandError> {
        let scope = match capability {
            Capability::CopilotApp => "read:user",
            Capability::PrRead | Capability::PrPublish => "repo read:org",
            Capability::DevicePending | Capability::CopilotCliOptOut => {
                return Err(invalid_capability());
            }
        };
        let response = reqwest::blocking::Client::new()
            .post("https://github.com/login/device/code")
            .header("Accept", "application/json")
            .form(&[("client_id", public_client_id), ("scope", scope)])
            .send()
            .and_then(reqwest::blocking::Response::error_for_status)
            .map_err(|_| ConnectionCommandError {
                code: "device_flow_unavailable".into(),
                message: "GitHub did not start the OAuth Device Flow.".into(),
                data_safety: "No bearer token was requested or stored.".into(),
                next_step: "Check the network connection and retry.".into(),
            })?;
        let response: GitHubDeviceCodeResponse =
            response.json().map_err(|_| ConnectionCommandError {
                code: "device_flow_invalid_response".into(),
                message: "GitHub returned an invalid Device Flow response.".into(),
                data_safety: "No bearer token was requested or stored.".into(),
                next_step: "Cancel this attempt and retry.".into(),
            })?;
        if response.device_code.is_empty()
            || response.user_code.is_empty()
            || response.verification_uri.is_empty()
            || response.expires_in == 0
            || response.interval == 0
        {
            return Err(ConnectionCommandError {
                code: "device_flow_invalid_response".into(),
                message: "GitHub returned an incomplete Device Flow response.".into(),
                data_safety: "No bearer token was requested or stored.".into(),
                next_step: "Cancel this attempt and retry.".into(),
            });
        }
        Ok(IssuedDeviceFlow {
            device_code: response.device_code,
            user_code: response.user_code,
            verification_uri: response.verification_uri,
            expires_in_seconds: response.expires_in,
            poll_interval_seconds: response.interval,
        })
    }
}

#[derive(Clone)]
pub enum DeviceFlowPollOutcome {
    Pending,
    SlowDown,
    Success {
        access_token: String,
        account_label: Option<String>,
        scopes: Vec<String>,
        expires_in_seconds: Option<u64>,
    },
    Expired,
    Denied,
}

impl fmt::Debug for DeviceFlowPollOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pending => f.write_str("Pending"),
            Self::SlowDown => f.write_str("SlowDown"),
            Self::Success {
                account_label,
                scopes,
                expires_in_seconds,
                ..
            } => f
                .debug_struct("Success")
                .field("access_token", &"[REDACTED]")
                .field("account_label", account_label)
                .field("scopes", scopes)
                .field("expires_in_seconds", expires_in_seconds)
                .finish(),
            Self::Expired => f.write_str("Expired"),
            Self::Denied => f.write_str("Denied"),
        }
    }
}

pub trait DeviceFlowPoller {
    fn poll(
        &self,
        public_client_id: &str,
        device_code: &str,
    ) -> Result<DeviceFlowPollOutcome, ConnectionCommandError>;
}

pub struct GitHubDeviceFlowPoller;

#[derive(Deserialize)]
struct GitHubTokenResponse {
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Deserialize)]
struct GitHubAccountResponse {
    login: String,
}

impl GitHubDeviceFlowPoller {
    fn public_account_label(&self, access_token: &str) -> Option<String> {
        let response = reqwest::blocking::Client::new()
            .get("https://api.github.com/user")
            .header("Accept", "application/vnd.github+json")
            .header("User-Agent", "Review-Queue")
            .bearer_auth(access_token)
            .send()
            .ok()?
            .error_for_status()
            .ok()?;
        let account: GitHubAccountResponse = response.json().ok()?;
        sanitize_account_label(&account.login)
    }
}

impl DeviceFlowPoller for GitHubDeviceFlowPoller {
    fn poll(
        &self,
        public_client_id: &str,
        device_code: &str,
    ) -> Result<DeviceFlowPollOutcome, ConnectionCommandError> {
        let response = reqwest::blocking::Client::new()
            .post("https://github.com/login/oauth/access_token")
            .header("Accept", "application/json")
            .form(&[
                ("client_id", public_client_id),
                ("device_code", device_code),
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ])
            .send()
            .and_then(reqwest::blocking::Response::error_for_status)
            .map_err(|_| ConnectionCommandError {
                code: "device_flow_poll_unavailable".into(),
                message: "GitHub could not check this Device Flow yet.".into(),
                data_safety: "The pending flow remains in Keychain. No credential was changed."
                    .into(),
                next_step: "Check the network connection and retry.".into(),
            })?;
        let response: GitHubTokenResponse =
            response.json().map_err(|_| ConnectionCommandError {
                code: "device_flow_invalid_response".into(),
                message: "GitHub returned an invalid Device Flow status.".into(),
                data_safety: "The pending flow remains in Keychain. No credential was changed."
                    .into(),
                next_step: "Cancel this attempt and start a new connection.".into(),
            })?;

        if let Some(access_token) = response.access_token {
            if access_token.is_empty() {
                return Err(ConnectionCommandError {
                    code: "device_flow_invalid_response".into(),
                    message: "GitHub returned an empty OAuth credential.".into(),
                    data_safety: "No credential was stored.".into(),
                    next_step: "Cancel this attempt and start a new connection.".into(),
                });
            }
            let account_label = self.public_account_label(&access_token);
            let scopes = response
                .scope
                .unwrap_or_default()
                .split([',', ' '])
                .map(str::trim)
                .filter(|scope| !scope.is_empty())
                .map(str::to_owned)
                .collect();
            return Ok(DeviceFlowPollOutcome::Success {
                access_token,
                account_label,
                scopes,
                expires_in_seconds: response.expires_in,
            });
        }

        Ok(match response.error.as_deref() {
            Some("authorization_pending") => DeviceFlowPollOutcome::Pending,
            Some("slow_down") => DeviceFlowPollOutcome::SlowDown,
            Some("expired_token") => DeviceFlowPollOutcome::Expired,
            Some("access_denied") => DeviceFlowPollOutcome::Denied,
            _ => {
                return Err(ConnectionCommandError {
                    code: "device_flow_provider_error".into(),
                    message: "GitHub could not complete this Device Flow.".into(),
                    data_safety:
                        "No OAuth credential was stored and the pending flow remains recoverable."
                            .into(),
                    next_step: "Cancel this attempt and start a new connection.".into(),
                });
            }
        })
    }
}

pub struct ConnectionService<B, P, I, D, C> {
    vault: CredentialVault<B>,
    cli_probe: P,
    issuer: I,
    poller: D,
    clock: C,
    bundled_public_client_id: Option<String>,
}

impl
    ConnectionService<
        MacOsKeychainBackend,
        SystemCopilotCliProbe,
        GitHubDeviceFlowIssuer,
        GitHubDeviceFlowPoller,
        SystemClock,
    >
{
    pub fn production() -> Self {
        Self {
            vault: CredentialVault::new(MacOsKeychainBackend::new()),
            cli_probe: SystemCopilotCliProbe,
            issuer: GitHubDeviceFlowIssuer,
            poller: GitHubDeviceFlowPoller,
            clock: SystemClock,
            bundled_public_client_id: BUNDLED_PUBLIC_CLIENT_ID.map(str::to_owned),
        }
    }
}

impl<B, P, I, D, C> ConnectionService<B, P, I, D, C>
where
    B: SecretBackend,
    P: CopilotCliProbe,
    I: DeviceFlowIssuer,
    D: DeviceFlowPoller,
    C: Clock,
{
    #[cfg(test)]
    fn new(vault: CredentialVault<B>, cli_probe: P, issuer: I, poller: D, clock: C) -> Self {
        Self {
            vault,
            cli_probe,
            issuer,
            poller,
            clock,
            bundled_public_client_id: Some("bundled-client".into()),
        }
    }

    pub fn health(&self) -> ConnectionHealth {
        let cli = self.cli_probe.validate_read_only();
        let cli_status = CliSignInStatus {
            installed: cli.installed,
            signed_in: cli.signed_in,
            account: cli.account.clone(),
            validation_is_read_only: true,
        };

        let keychain_result = self.read_keychain_state();
        let Ok((copilot_app, pr_read, pr_publish, opted_out, client_id, pending)) = keychain_result
        else {
            return ConnectionHealth {
                cli: cli_status,
                copilot: if cli.signed_in {
                    capability_status(
                        Capability::CopilotApp,
                        ConnectionState::Connected,
                        ConnectionSource::ExistingCopilotCli,
                        cli.account,
                        Vec::new(),
                        None,
                    )
                } else {
                    unavailable_status(Capability::CopilotApp)
                },
                pr_read: unavailable_status(Capability::PrRead),
                pr_publish: unavailable_status(Capability::PrPublish),
                keychain: KeychainHealth {
                    available: false,
                    service: crate::keychain_vault::KEYCHAIN_SERVICE.into(),
                    recovery_instructions: Some(KEYCHAIN_RECOVERY.into()),
                },
                public_client_id: self.bundled_public_client_id.clone(),
                pending_device_flow: None,
            };
        };

        let copilot = if cli.signed_in && !opted_out {
            capability_status(
                Capability::CopilotApp,
                ConnectionState::Connected,
                ConnectionSource::ExistingCopilotCli,
                cli.account,
                Vec::new(),
                None,
            )
        } else if let Some(copilot_app) = copilot_app {
            app_connected_status(Capability::CopilotApp, copilot_app)
        } else {
            capability_status(
                Capability::CopilotApp,
                ConnectionState::NotConnected,
                ConnectionSource::None,
                None,
                Vec::new(),
                None,
            )
        };

        ConnectionHealth {
            cli: cli_status,
            copilot,
            pr_read: app_status(Capability::PrRead, pr_read),
            pr_publish: app_status(Capability::PrPublish, pr_publish),
            keychain: KeychainHealth {
                available: true,
                service: crate::keychain_vault::KEYCHAIN_SERVICE.into(),
                recovery_instructions: None,
            },
            public_client_id: client_id.or_else(|| self.bundled_public_client_id.clone()),
            pending_device_flow: pending,
        }
    }

    #[allow(clippy::type_complexity)]
    fn read_keychain_state(
        &self,
    ) -> Result<
        (
            Option<AppCredentialRecord>,
            Option<AppCredentialRecord>,
            Option<AppCredentialRecord>,
            bool,
            Option<String>,
            Option<DeviceFlowPublicState>,
        ),
        VaultError,
    > {
        let copilot = self.vault.get_app_credential(Capability::CopilotApp)?;
        let pr_read = self.vault.get_app_credential(Capability::PrRead)?;
        let pr_publish = self.vault.get_app_credential(Capability::PrPublish)?;
        let opted_out = self.vault.get(Capability::CopilotCliOptOut)?.is_some();
        let client_id = self.vault.public_client_id()?;
        let pending = self.pending_public_state()?;
        Ok((copilot, pr_read, pr_publish, opted_out, client_id, pending))
    }

    fn pending_public_state(&self) -> Result<Option<DeviceFlowPublicState>, VaultError> {
        let Some(pending) = self.vault.get_device_pending()? else {
            return Ok(None);
        };
        let remaining = pending
            .expires_at_unix_seconds
            .saturating_sub(self.clock.unix_seconds());
        let expired = remaining <= 0;
        if expired {
            self.vault.delete(Capability::DevicePending)?;
        }
        Ok(Some(DeviceFlowPublicState {
            capability: pending.target.account().into(),
            user_code: pending.user_code,
            verification_uri: pending.verification_uri,
            expires_at_unix_seconds: pending.expires_at_unix_seconds,
            seconds_remaining: remaining.max(0) as u64,
            phase: if expired {
                DeviceFlowPhase::Expired
            } else {
                DeviceFlowPhase::Pending
            },
            can_cancel: !expired,
        }))
    }

    pub fn disconnect(
        &self,
        request: DisconnectCapabilityRequest,
    ) -> Result<ConnectionHealth, ConnectionCommandError> {
        let capability =
            Capability::from_str(&request.capability).map_err(|_| invalid_capability())?;
        match request.source {
            ConnectionSource::ExistingCopilotCli if capability == Capability::CopilotApp => {
                // This is an app preference only. It cannot log out the CLI or
                // remove/change the CLI's credential.
                self.vault
                    .set(Capability::CopilotCliOptOut, "disabled-by-user")?;
            }
            ConnectionSource::AppOwnedOauth
                if matches!(
                    capability,
                    Capability::CopilotApp | Capability::PrRead | Capability::PrPublish
                ) =>
            {
                self.vault.delete(capability)?;
            }
            _ => return Err(invalid_capability()),
        }
        Ok(self.health())
    }

    pub fn start_device_flow(
        &self,
        request: StartDeviceFlowRequest,
    ) -> Result<DeviceFlowPublicState, ConnectionCommandError> {
        let capability =
            Capability::from_str(&request.capability).map_err(|_| invalid_capability())?;
        if !matches!(
            capability,
            Capability::CopilotApp | Capability::PrRead | Capability::PrPublish
        ) {
            return Err(invalid_capability());
        }
        let expected_account = match request.expected_account {
            Some(account) => Some(sanitize_account_label(&account).ok_or_else(|| {
                ConnectionCommandError {
                    code: "invalid_expected_account".into(),
                    message: "The expected GitHub account label is invalid.".into(),
                    data_safety: "No Device Flow or credential was created.".into(),
                    next_step: "Remove the account expectation or enter a public account label."
                        .into(),
                }
            })?),
            None => None,
        };
        let client_id = self
            .vault
            .public_client_id()?
            .or_else(|| self.bundled_public_client_id.clone())
            .ok_or_else(|| ConnectionCommandError {
                code: "oauth_public_client_id_required".into(),
                message: "No GitHub OAuth public client ID is configured.".into(),
                data_safety: "No credential or review data was changed.".into(),
                next_step: "Set a public Client ID in Settings. Never paste a client secret or bearer token."
                    .into(),
            })?;
        self.vault.initialize_public_client_id(&client_id)?;
        let issued = self.issuer.start(&client_id, capability)?;
        let expires_at = self
            .clock
            .unix_seconds()
            .saturating_add(issued.expires_in_seconds as i64);
        let pending = DevicePendingState {
            public_client_id: client_id,
            target: capability,
            expected_account_label: expected_account,
            device_code: issued.device_code,
            user_code: issued.user_code,
            verification_uri: issued.verification_uri,
            expires_at_unix_seconds: expires_at,
            poll_interval_seconds: issued.poll_interval_seconds,
        };
        self.vault.set_device_pending(&pending)?;
        self.pending_public_state()?
            .ok_or_else(|| ConnectionCommandError {
                code: "device_flow_not_saved".into(),
                message: "The pending Device Flow could not be recovered.".into(),
                data_safety: "No bearer token was requested or stored.".into(),
                next_step: "Retry the connection after checking Keychain access.".into(),
            })
    }

    pub fn cancel_device_flow(&self) -> Result<ConnectionHealth, ConnectionCommandError> {
        self.vault.delete(Capability::DevicePending)?;
        Ok(self.health())
    }

    pub fn complete_device_flow(
        &self,
    ) -> Result<DeviceFlowPollPublicResult, ConnectionCommandError> {
        let mut pending =
            self.vault
                .get_device_pending()?
                .ok_or_else(|| ConnectionCommandError {
                    code: "device_flow_not_pending".into(),
                    message: "There is no pending OAuth Device Flow.".into(),
                    data_safety: "No credential or review data was changed.".into(),
                    next_step: "Start a connection before checking browser approval.".into(),
                })?;
        let remaining = pending
            .expires_at_unix_seconds
            .saturating_sub(self.clock.unix_seconds());
        if remaining <= 0 {
            self.vault.delete(Capability::DevicePending)?;
            return Ok(poll_public(
                &pending,
                DeviceFlowPhase::Expired,
                None,
                0,
                None,
                "This public code expired. Start a new connection.",
            ));
        }

        match self
            .poller
            .poll(&pending.public_client_id, &pending.device_code)?
        {
            DeviceFlowPollOutcome::Pending => Ok(poll_public(
                &pending,
                DeviceFlowPhase::Pending,
                None,
                remaining as u64,
                Some(pending.poll_interval_seconds),
                "Browser approval is still pending.",
            )),
            DeviceFlowPollOutcome::SlowDown => {
                pending.poll_interval_seconds = pending.poll_interval_seconds.saturating_add(5);
                self.vault.set_device_pending(&pending)?;
                Ok(poll_public(
                    &pending,
                    DeviceFlowPhase::SlowDown,
                    None,
                    remaining as u64,
                    Some(pending.poll_interval_seconds),
                    "GitHub asked Review Queue to check less often.",
                ))
            }
            DeviceFlowPollOutcome::Expired => {
                self.vault.delete(Capability::DevicePending)?;
                Ok(poll_public(
                    &pending,
                    DeviceFlowPhase::Expired,
                    None,
                    0,
                    None,
                    "GitHub expired this public code. Start a new connection.",
                ))
            }
            DeviceFlowPollOutcome::Denied => {
                self.vault.delete(Capability::DevicePending)?;
                Ok(poll_public(
                    &pending,
                    DeviceFlowPhase::Denied,
                    None,
                    remaining as u64,
                    None,
                    "GitHub denied this connection. Start again when you are ready.",
                ))
            }
            DeviceFlowPollOutcome::Success {
                access_token,
                account_label,
                scopes,
                expires_in_seconds,
            } => {
                let mismatch = pending
                    .expected_account_label
                    .as_ref()
                    .is_some_and(|expected| {
                        account_label
                            .as_ref()
                            .is_none_or(|actual| !actual.eq_ignore_ascii_case(expected))
                    });
                if mismatch {
                    self.vault.delete(Capability::DevicePending)?;
                    return Ok(poll_public(
                        &pending,
                        DeviceFlowPhase::AccountMismatch,
                        account_label,
                        0,
                        None,
                        "GitHub approved a different account, or its account could not be verified. Reconnect with the expected account.",
                    ));
                }
                let expires_at_unix_seconds = expires_in_seconds
                    .map(|seconds| self.clock.unix_seconds().saturating_add(seconds as i64));
                self.vault.set_app_credential(
                    pending.target,
                    &AppCredentialRecord {
                        access_token,
                        account_label: account_label.clone(),
                        scopes,
                        expires_at_unix_seconds,
                    },
                )?;
                if pending.target == Capability::CopilotApp {
                    self.vault
                        .set(Capability::CopilotCliOptOut, "app-oauth-selected")?;
                }
                self.vault.delete(Capability::DevicePending)?;
                Ok(poll_public(
                    &pending,
                    DeviceFlowPhase::Connected,
                    account_label,
                    0,
                    None,
                    "The app-owned capability is connected.",
                ))
            }
        }
    }

    pub fn change_public_client_id(
        &self,
        request: ChangePublicClientIdRequest,
    ) -> Result<ChangePublicClientIdResult, ConnectionCommandError> {
        let new_id = request.public_client_id.trim();
        if new_id.is_empty()
            || new_id.len() > 256
            || !new_id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
            || review_queue_core::redact_for_diagnostics(new_id).is_err()
        {
            return Err(ConnectionCommandError {
                code: "invalid_public_client_id".into(),
                message: "The OAuth public client ID is invalid.".into(),
                data_safety: "No connection or review data was changed.".into(),
                next_step:
                    "Enter the public Client ID only. Never enter a client secret or bearer token."
                        .into(),
            });
        }
        let current = self
            .vault
            .public_client_id()?
            .or_else(|| self.bundled_public_client_id.clone());
        if current.as_deref() == Some(new_id) {
            return Ok(ChangePublicClientIdResult {
                changed: false,
                app_owned_records_cleared: false,
                sqlite_preserved: true,
            });
        }
        if current.is_some() && !request.confirmed {
            return Err(ConnectionCommandError {
                code: "public_client_id_change_confirmation_required".into(),
                message:
                    "Changing the public client ID disconnects app-owned connections and cancels the pending Device Flow."
                        .into(),
                data_safety:
                    "The existing client ID, Keychain records, snapshots, comments, and SQLite data are unchanged."
                        .into(),
                next_step: "Confirm the change in Settings to continue.".into(),
            });
        }
        let cleared = self.vault.invalidate_for_public_client_id(new_id)?;
        Ok(ChangePublicClientIdResult {
            changed: true,
            app_owned_records_cleared: cleared,
            sqlite_preserved: true,
        })
    }
}

fn invalid_capability() -> ConnectionCommandError {
    ConnectionCommandError {
        code: "invalid_connection_capability".into(),
        message: "The requested connection capability or source is not supported.".into(),
        data_safety: "No credential or review data was changed.".into(),
        next_step: "Choose Copilot app, PR read, or PR publish.".into(),
    }
}

fn unavailable_status(capability: Capability) -> CapabilityStatus {
    capability_status(
        capability,
        ConnectionState::Unavailable,
        ConnectionSource::None,
        None,
        Vec::new(),
        None,
    )
}

fn app_status(capability: Capability, credential: Option<AppCredentialRecord>) -> CapabilityStatus {
    match credential {
        Some(credential) => app_connected_status(capability, credential),
        None => capability_status(
            capability,
            ConnectionState::NotConnected,
            ConnectionSource::None,
            None,
            Vec::new(),
            None,
        ),
    }
}

fn app_connected_status(
    capability: Capability,
    credential: AppCredentialRecord,
) -> CapabilityStatus {
    capability_status(
        capability,
        ConnectionState::Connected,
        ConnectionSource::AppOwnedOauth,
        credential.account_label,
        credential.scopes,
        credential.expires_at_unix_seconds,
    )
}

fn capability_status(
    capability: Capability,
    state: ConnectionState,
    source: ConnectionSource,
    account: Option<String>,
    scopes: Vec<String>,
    expires_at_unix_seconds: Option<i64>,
) -> CapabilityStatus {
    let (optional, explanation) = match capability {
        Capability::CopilotApp => (
            true,
            "Used only for explicit /ask prompts. Existing CLI sign-in and app OAuth remain distinct."
                .into(),
        ),
        Capability::PrRead => (
            true,
            "Reads pull-request metadata, commits, and review threads for local review.".into(),
        ),
        Capability::PrPublish => (
            true,
            "Required only when you explicitly publish a GitHub review.".into(),
        ),
        Capability::DevicePending | Capability::CopilotCliOptOut => {
            (true, "Internal connection state.".into())
        }
    };
    CapabilityStatus {
        capability: capability.account().into(),
        state,
        source,
        account,
        scopes,
        expires_at_unix_seconds,
        optional,
        explanation,
    }
}

fn poll_public(
    pending: &DevicePendingState,
    phase: DeviceFlowPhase,
    account: Option<String>,
    seconds_remaining: u64,
    retry_after_seconds: Option<u64>,
    message: &str,
) -> DeviceFlowPollPublicResult {
    DeviceFlowPollPublicResult {
        capability: pending.target.account().into(),
        phase,
        account,
        seconds_remaining,
        retry_after_seconds,
        message: message.into(),
    }
}

#[tauri::command]
pub fn connection_status() -> ConnectionHealth {
    ConnectionService::production().health()
}

#[tauri::command]
pub fn retry_connection() -> ConnectionHealth {
    ConnectionService::production().health()
}

#[tauri::command]
pub fn disconnect_capability(
    request: DisconnectCapabilityRequest,
) -> Result<ConnectionHealth, ConnectionCommandError> {
    ConnectionService::production().disconnect(request)
}

#[tauri::command]
pub fn start_device_flow(
    request: StartDeviceFlowRequest,
) -> Result<DeviceFlowPublicState, ConnectionCommandError> {
    ConnectionService::production().start_device_flow(request)
}

#[tauri::command]
pub fn cancel_device_flow() -> Result<ConnectionHealth, ConnectionCommandError> {
    ConnectionService::production().cancel_device_flow()
}

#[tauri::command]
pub fn complete_device_flow() -> Result<DeviceFlowPollPublicResult, ConnectionCommandError> {
    ConnectionService::production().complete_device_flow()
}

#[tauri::command]
pub fn set_public_client_id(
    request: ChangePublicClientIdRequest,
) -> Result<ChangePublicClientIdResult, ConnectionCommandError> {
    ConnectionService::production().change_public_client_id(request)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, collections::BTreeMap};

    #[derive(Default)]
    struct MemoryBackend(RefCell<BTreeMap<String, String>>);

    impl SecretBackend for MemoryBackend {
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

    struct FixedProbe(CliProbeResult);

    impl CopilotCliProbe for FixedProbe {
        fn validate_read_only(&self) -> CliProbeResult {
            self.0.clone()
        }
    }

    struct FixedIssuer;

    impl DeviceFlowIssuer for FixedIssuer {
        fn start(
            &self,
            _public_client_id: &str,
            _capability: Capability,
        ) -> Result<IssuedDeviceFlow, ConnectionCommandError> {
            Ok(IssuedDeviceFlow {
                device_code: "secret-device-code".into(),
                user_code: "PUBLIC-CODE".into(),
                verification_uri: "https://github.com/login/device".into(),
                expires_in_seconds: 600,
                poll_interval_seconds: 5,
            })
        }
    }

    struct FixedPoller(DeviceFlowPollOutcome);

    impl DeviceFlowPoller for FixedPoller {
        fn poll(
            &self,
            _public_client_id: &str,
            _device_code: &str,
        ) -> Result<DeviceFlowPollOutcome, ConnectionCommandError> {
            Ok(self.0.clone())
        }
    }

    struct FixedClock(i64);

    impl Clock for FixedClock {
        fn unix_seconds(&self) -> i64 {
            self.0
        }
    }

    struct UnavailableBackend;

    impl SecretBackend for UnavailableBackend {
        fn set(&self, _account: &str, _value: &str) -> Result<(), VaultError> {
            Err(VaultError::Unavailable)
        }

        fn get(&self, _account: &str) -> Result<Option<String>, VaultError> {
            Err(VaultError::Unavailable)
        }

        fn delete(&self, _account: &str) -> Result<(), VaultError> {
            Err(VaultError::Unavailable)
        }
    }

    fn service(
        signed_in: bool,
    ) -> ConnectionService<MemoryBackend, FixedProbe, FixedIssuer, FixedPoller, FixedClock> {
        ConnectionService::new(
            CredentialVault::new(MemoryBackend::default()),
            FixedProbe(CliProbeResult {
                installed: true,
                signed_in,
                account: signed_in.then(|| "person@example.com".into()),
            }),
            FixedIssuer,
            FixedPoller(DeviceFlowPollOutcome::Pending),
            FixedClock(1_700_000_000),
        )
    }

    #[test]
    fn existing_cli_probe_is_distinct_and_disconnect_never_changes_cli_credentials() {
        let service = service(true);
        let health = service.health();
        assert_eq!(health.copilot.source, ConnectionSource::ExistingCopilotCli);
        assert_eq!(
            health.copilot.account.as_deref(),
            Some("person@example.com")
        );
        let health = service
            .disconnect(DisconnectCapabilityRequest {
                capability: "copilot_app".into(),
                source: ConnectionSource::ExistingCopilotCli,
            })
            .unwrap();
        assert_eq!(health.copilot.state, ConnectionState::NotConnected);
        assert!(health.cli.signed_in);
    }

    #[test]
    fn pr_read_and_publish_are_separate_keychain_accounts() {
        let service = service(false);
        service
            .vault
            .set(Capability::PrRead, "opaque-read-credential")
            .unwrap();
        let health = service.health();
        assert_eq!(health.pr_read.state, ConnectionState::Connected);
        assert_eq!(health.pr_publish.state, ConnectionState::NotConnected);
    }

    #[test]
    fn unavailable_keychain_has_exact_actionable_unlock_instructions() {
        let service = ConnectionService::new(
            CredentialVault::new(UnavailableBackend),
            FixedProbe(CliProbeResult {
                installed: false,
                signed_in: false,
                account: None,
            }),
            FixedIssuer,
            FixedPoller(DeviceFlowPollOutcome::Pending),
            FixedClock(1_700_000_000),
        );
        let health = service.health();
        assert!(!health.keychain.available);
        let instructions = health.keychain.recovery_instructions.unwrap();
        assert!(instructions.contains("Open Keychain Access"));
        assert!(instructions.contains("select the login keychain"));
        assert!(instructions.contains("unlock it"));
        assert!(instructions.contains("Retry connection"));
        assert_eq!(health.pr_read.state, ConnectionState::Unavailable);
        assert_eq!(health.pr_publish.state, ConnectionState::Unavailable);
    }

    #[test]
    fn device_flow_ipc_has_public_fields_but_never_secret_or_bearer_input() {
        let service = service(false);
        let public = service
            .start_device_flow(StartDeviceFlowRequest {
                capability: "pr_read".into(),
                expected_account: None,
            })
            .unwrap();
        assert_eq!(public.user_code, "PUBLIC-CODE");
        assert_eq!(public.seconds_remaining, 600);
        let json = serde_json::to_string(&public).unwrap();
        assert!(!json.contains("secret-device-code"));
        assert!(!json.to_ascii_lowercase().contains("bearer"));
        let pending = service.vault.get_device_pending().unwrap().unwrap();
        assert_eq!(pending.device_code, "secret-device-code");
        assert_eq!(pending.target, Capability::PrRead);
        let cancelled = service.cancel_device_flow().unwrap();
        assert!(cancelled.pending_device_flow.is_none());
    }

    #[test]
    fn expired_device_flow_is_reported_once_and_removed() {
        let service = service(false);
        service
            .start_device_flow(StartDeviceFlowRequest {
                capability: "copilot_app".into(),
                expected_account: None,
            })
            .unwrap();
        let mut service = service;
        service.clock = FixedClock(1_700_000_601);
        let health = service.health();
        assert_eq!(
            health.pending_device_flow.unwrap().phase,
            DeviceFlowPhase::Expired
        );
        assert!(service.vault.get_device_pending().unwrap().is_none());
    }

    #[test]
    fn public_client_change_requires_confirmation_and_preserves_non_keychain_data() {
        let service = service(false);
        service
            .vault
            .initialize_public_client_id("bundled-client")
            .unwrap();
        service
            .vault
            .set(Capability::PrPublish, "opaque-publish-credential")
            .unwrap();
        let error = service
            .change_public_client_id(ChangePublicClientIdRequest {
                public_client_id: "custom-client".into(),
                confirmed: false,
            })
            .unwrap_err();
        assert_eq!(error.code, "public_client_id_change_confirmation_required");
        assert!(service.vault.get(Capability::PrPublish).unwrap().is_some());
        let result = service
            .change_public_client_id(ChangePublicClientIdRequest {
                public_client_id: "custom-client".into(),
                confirmed: true,
            })
            .unwrap();
        assert!(result.app_owned_records_cleared);
        assert!(result.sqlite_preserved);
        assert!(service.vault.get(Capability::PrPublish).unwrap().is_none());
    }

    #[test]
    fn token_shaped_value_is_rejected_as_a_public_client_id() {
        let service = service(false);
        let error = service
            .change_public_client_id(ChangePublicClientIdRequest {
                public_client_id: "github_pat_secretish".into(),
                confirmed: true,
            })
            .unwrap_err();
        assert_eq!(error.code, "invalid_public_client_id");
        assert!(service.vault.public_client_id().unwrap().is_none());
    }

    #[test]
    fn copilot_keychain_parser_reads_only_the_public_account_attribute() {
        let metadata = br#"
keychain: "/Users/person/Library/Keychains/login.keychain-db"
attributes:
    "acct"<blob>="https://github.com:octocat"
    "svce"<blob>="copilot-cli"
"#;
        assert_eq!(parse_keychain_account(metadata), Some("octocat".into()));
        assert_eq!(
            parse_keychain_account(br#""svce"<blob>="copilot-cli""#),
            None
        );
    }

    #[test]
    fn pending_poll_keeps_secret_state_and_returns_only_public_metadata() {
        let service = service(false);
        service
            .start_device_flow(StartDeviceFlowRequest {
                capability: "pr_read".into(),
                expected_account: None,
            })
            .unwrap();
        let result = service.complete_device_flow().unwrap();
        assert_eq!(result.phase, DeviceFlowPhase::Pending);
        assert_eq!(result.retry_after_seconds, Some(5));
        assert!(service.vault.get_device_pending().unwrap().is_some());
        let json = serde_json::to_string(&result).unwrap();
        assert!(!json.contains("secret-device-code"));
        assert!(!json.to_ascii_lowercase().contains("access_token"));
    }

    #[test]
    fn slow_down_increases_public_retry_delay_and_keeps_pending_state() {
        let mut service = service(false);
        service
            .start_device_flow(StartDeviceFlowRequest {
                capability: "pr_read".into(),
                expected_account: None,
            })
            .unwrap();
        service.poller.0 = DeviceFlowPollOutcome::SlowDown;
        let result = service.complete_device_flow().unwrap();
        assert_eq!(result.phase, DeviceFlowPhase::SlowDown);
        assert_eq!(result.retry_after_seconds, Some(10));
        assert_eq!(
            service
                .vault
                .get_device_pending()
                .unwrap()
                .unwrap()
                .poll_interval_seconds,
            10
        );
    }

    #[test]
    fn successful_poll_stores_token_only_in_original_capability_and_clears_pending() {
        let mut service = service(false);
        service
            .start_device_flow(StartDeviceFlowRequest {
                capability: "pr_publish".into(),
                expected_account: Some("octocat".into()),
            })
            .unwrap();
        service.poller.0 = DeviceFlowPollOutcome::Success {
            access_token: "github-secret-access-token".into(),
            account_label: Some("octocat".into()),
            scopes: vec!["repo".into()],
            expires_in_seconds: Some(3600),
        };
        let result = service.complete_device_flow().unwrap();
        assert_eq!(result.phase, DeviceFlowPhase::Connected);
        assert_eq!(result.account.as_deref(), Some("octocat"));
        assert!(service.vault.get_device_pending().unwrap().is_none());
        assert!(
            service
                .vault
                .get_app_credential(Capability::PrPublish)
                .unwrap()
                .is_some()
        );
        assert!(
            service
                .vault
                .get_app_credential(Capability::PrRead)
                .unwrap()
                .is_none()
        );
        assert!(
            service
                .vault
                .get_app_credential(Capability::CopilotApp)
                .unwrap()
                .is_none()
        );
        let serialized = serde_json::to_string(&result).unwrap();
        assert!(!serialized.contains("github-secret-access-token"));
        assert!(!format!("{result:?}").contains("github-secret-access-token"));
        assert!(!format!("{:?}", service.poller.0).contains("github-secret-access-token"));
        let health = service.health();
        assert_eq!(health.pr_publish.account.as_deref(), Some("octocat"));
        assert_eq!(health.pr_publish.scopes, vec!["repo"]);
    }

    #[test]
    fn provider_expiry_and_denial_clear_pending_without_storing_a_token() {
        for outcome in [
            DeviceFlowPollOutcome::Expired,
            DeviceFlowPollOutcome::Denied,
        ] {
            let mut service = service(false);
            service
                .start_device_flow(StartDeviceFlowRequest {
                    capability: "pr_read".into(),
                    expected_account: None,
                })
                .unwrap();
            service.poller.0 = outcome;
            let result = service.complete_device_flow().unwrap();
            assert!(matches!(
                result.phase,
                DeviceFlowPhase::Expired | DeviceFlowPhase::Denied
            ));
            assert!(service.vault.get_device_pending().unwrap().is_none());
            assert!(
                service
                    .vault
                    .get_app_credential(Capability::PrRead)
                    .unwrap()
                    .is_none()
            );
        }
    }

    #[test]
    fn account_mismatch_discards_returned_token_and_requires_recovery() {
        let mut service = service(false);
        service
            .start_device_flow(StartDeviceFlowRequest {
                capability: "copilot_app".into(),
                expected_account: Some("expected-user".into()),
            })
            .unwrap();
        service.poller.0 = DeviceFlowPollOutcome::Success {
            access_token: "wrong-account-secret-token".into(),
            account_label: Some("different-user".into()),
            scopes: vec!["read:user".into()],
            expires_in_seconds: None,
        };
        let result = service.complete_device_flow().unwrap();
        assert_eq!(result.phase, DeviceFlowPhase::AccountMismatch);
        assert_eq!(result.account.as_deref(), Some("different-user"));
        assert!(service.vault.get_device_pending().unwrap().is_none());
        assert!(
            service
                .vault
                .get_app_credential(Capability::CopilotApp)
                .unwrap()
                .is_none()
        );
        assert!(
            !serde_json::to_string(&result)
                .unwrap()
                .contains("wrong-account-secret-token")
        );
    }

    #[test]
    fn public_device_state_debug_is_redacted() {
        let service = service(false);
        let state = service
            .start_device_flow(StartDeviceFlowRequest {
                capability: "pr_publish".into(),
                expected_account: None,
            })
            .unwrap();
        assert!(!format!("{state:?}").contains("PUBLIC-CODE"));
    }

    #[test]
    fn cli_health_probe_never_uses_gh_or_the_missing_copilot_auth_status_command() {
        let source = include_str!("connection_health.rs");
        assert!(!source.contains("Command::new(\"gh\")"));
        assert!(!source.contains(".args([\"auth\", \"status\"])"));
        assert!(
            !source
                .contains("Command::new(\"copilot\")\n            .args([\"auth\", \"status\"])")
        );
    }
}
