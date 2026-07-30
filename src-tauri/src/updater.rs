//! Signed updater operations exposed to the webview.
//!
//! Checking is read-only. Download/install and relaunch are separate,
//! explicitly confirmed commands so the UI can show the exact version and
//! never surprise the reviewer with an application restart.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::AppHandle;
use tauri_plugin_updater::UpdaterExt;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateCheck {
    pub available: bool,
    pub current_version: String,
    pub version: Option<String>,
    pub date: Option<String>,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallUpdateRequest {
    pub expected_version: String,
    pub confirmed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInstall {
    pub installed: bool,
    pub version: String,
    pub relaunch_required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UpdaterError {
    pub code: String,
    pub message: String,
    pub data_safety: String,
    pub next_step: String,
}

fn updater_error(code: &str, message: &str, next_step: &str) -> UpdaterError {
    UpdaterError {
        code: code.to_owned(),
        message: message.to_owned(),
        data_safety: "The running app and saved review data are unchanged.".to_owned(),
        next_step: next_step.to_owned(),
    }
}

#[tauri::command]
pub async fn check_for_update(app: AppHandle) -> Result<UpdateCheck, UpdaterError> {
    let current_version = app.package_info().version.to_string();
    let updater = app
        .updater_builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|_| {
            updater_error(
                "updater_initialization_failed",
                "Review Queue could not initialize its signed updater.",
                "Check the updater configuration and retry.",
            )
        })?;
    let update = updater.check().await.map_err(|_| {
        updater_error(
            "update_check_failed",
            "Review Queue could not check the signed update feed.",
            "Check the network connection and retry.",
        )
    })?;

    Ok(match update {
        Some(update) => UpdateCheck {
            available: true,
            current_version,
            version: Some(update.version.to_string()),
            date: update.date.map(|date| date.to_string()),
            notes: update.body,
        },
        None => UpdateCheck {
            available: false,
            current_version,
            version: None,
            date: None,
            notes: None,
        },
    })
}

#[tauri::command]
pub async fn install_update(
    app: AppHandle,
    request: InstallUpdateRequest,
) -> Result<UpdateInstall, UpdaterError> {
    if !request.confirmed || request.expected_version.trim().is_empty() {
        return Err(updater_error(
            "update_confirmation_required",
            "Installing an update requires confirmation of the displayed version.",
            "Review the version and release notes, then confirm installation.",
        ));
    }
    let updater = app
        .updater_builder()
        .timeout(Duration::from_secs(60))
        .build()
        .map_err(|_| {
            updater_error(
                "updater_initialization_failed",
                "Review Queue could not initialize its signed updater.",
                "Check the updater configuration and retry.",
            )
        })?;
    let update = updater
        .check()
        .await
        .map_err(|_| {
            updater_error(
                "update_check_failed",
                "Review Queue could not re-check the signed update feed.",
                "Check the network connection and retry.",
            )
        })?
        .ok_or_else(|| {
            updater_error(
                "update_no_longer_available",
                "The selected update is no longer available.",
                "Check for updates again.",
            )
        })?;
    if update.version != request.expected_version {
        return Err(updater_error(
            "update_version_changed",
            "The available update changed after the confirmation dialog opened.",
            "Review the new version and release notes before confirming again.",
        ));
    }
    update
        .download_and_install(|_, _| {}, || {})
        .await
        .map_err(|_| {
            updater_error(
                "update_install_failed",
                "The signed update could not be downloaded or installed.",
                "Keep this version open, check the connection, and retry.",
            )
        })?;
    Ok(UpdateInstall {
        installed: true,
        version: request.expected_version,
        relaunch_required: true,
    })
}

#[tauri::command]
pub fn relaunch_after_update(app: AppHandle, confirmed: bool) -> Result<(), UpdaterError> {
    if !confirmed {
        return Err(updater_error(
            "relaunch_confirmation_required",
            "Relaunching after an update requires confirmation.",
            "Save any in-progress text, then confirm relaunch.",
        ));
    }
    app.restart()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn updater_errors_never_claim_review_data_changed() {
        let error = updater_error("test", "message", "retry");
        assert_eq!(
            error.data_safety,
            "The running app and saved review data are unchanged."
        );
    }
}
