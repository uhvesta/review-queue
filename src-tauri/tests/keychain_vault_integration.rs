//! Deliberately opt-in: this touches the current user's login Keychain.

#![cfg(target_os = "macos")]

use review_queue_desktop::keychain_vault::{Capability, CredentialVault, MacOsKeychainBackend};

struct DisposableKeychainItem {
    vault: CredentialVault<MacOsKeychainBackend>,
}

impl Drop for DisposableKeychainItem {
    fn drop(&mut self) {
        // This service name is generated solely for this test. Best-effort
        // cleanup still runs when an assertion panics, and never touches the
        // production Review Queue service or any user credential.
        let _ = self.vault.delete(Capability::PrRead);
    }
}

#[test]
#[ignore = "uses the macOS login Keychain; run explicitly on a signed desktop host"]
fn disposable_named_keychain_item_round_trips() {
    let service = format!(
        "com.reviewqueue.desktop.acceptance.{}.{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let item = DisposableKeychainItem {
        vault: CredentialVault::new(MacOsKeychainBackend::with_service(service)),
    };

    item.vault.delete(Capability::PrRead).unwrap();
    item.vault
        .set(Capability::PrRead, "acceptance-marker")
        .unwrap();
    assert_eq!(
        item.vault.get(Capability::PrRead).unwrap().as_deref(),
        Some("acceptance-marker")
    );
    item.vault.delete(Capability::PrRead).unwrap();
    assert!(item.vault.get(Capability::PrRead).unwrap().is_none());
}
