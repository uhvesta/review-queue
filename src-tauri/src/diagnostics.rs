//! Metadata-only support artifacts.
//!
//! The core scans every persisted text/blob value for token-shaped content
//! before this module writes anything. The resulting file contains counts and
//! build metadata only—never database rows, environment variables, command
//! output, credentials, prompts, or source.

use std::{fs, os::unix::fs::PermissionsExt, process::Command};

use chrono::Utc;
use review_queue_core::store::RedactedArtifactExport;
use serde::Serialize;
use tauri::{AppHandle, Manager, State};

use crate::commands::{AppState, CommandError};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticsExport {
    pub path: String,
    pub artifact: RedactedArtifactExport,
}

#[tauri::command]
pub fn export_redacted_diagnostics(
    app: AppHandle,
    open_in_finder: bool,
    state: State<'_, AppState>,
) -> Result<DiagnosticsExport, CommandError> {
    let artifact = state
        .0
        .lock()
        .map_err(|_| unavailable())?
        .export_redacted_artifact(&serde_json::json!({
            "application": "review-queue",
            "version": app.package_info().version.to_string(),
            "platform": std::env::consts::OS,
            "architecture": std::env::consts::ARCH,
        }))?;
    let directory = app
        .path()
        .app_data_dir()
        .map_err(|_| {
            filesystem_error("Review Queue could not locate its application data directory.")
        })?
        .join("diagnostics");
    fs::create_dir_all(&directory).map_err(|_| {
        filesystem_error("Review Queue could not create its diagnostics directory.")
    })?;
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).map_err(|_| {
        filesystem_error("Review Queue could not secure its diagnostics directory.")
    })?;
    let path = directory.join(format!(
        "review-queue-redacted-{}.json",
        Utc::now().format("%Y%m%dT%H%M%SZ")
    ));
    let bytes = serde_json::to_vec_pretty(&artifact)
        .map_err(|_| filesystem_error("Review Queue could not serialize redacted diagnostics."))?;
    fs::write(&path, bytes)
        .map_err(|_| filesystem_error("Review Queue could not write redacted diagnostics."))?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
        .map_err(|_| filesystem_error("Review Queue could not secure redacted diagnostics."))?;

    if open_in_finder {
        let _ = Command::new("/usr/bin/open")
            .arg("-R")
            .arg(&path)
            .env_clear()
            .spawn();
    }
    Ok(DiagnosticsExport {
        path: path.to_string_lossy().into_owned(),
        artifact,
    })
}

fn unavailable() -> CommandError {
    CommandError {
        code: "store_unavailable".into(),
        message: "Review Queue could not open its local review store.".into(),
        data_safety: "No diagnostic was exported and no review data changed.".into(),
        next_step: "Retry; if the app is closing, reopen it first.".into(),
    }
}

fn filesystem_error(message: &str) -> CommandError {
    CommandError {
        code: "diagnostics_export_failed".into(),
        message: message.into(),
        data_safety: "No credential or review content was exported.".into(),
        next_step: "Check application-data permissions and retry.".into(),
    }
}
