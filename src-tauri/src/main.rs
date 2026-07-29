//! Tauri host boundary for the Review Queue React shell.
//!
//! Browser code may call only the commands registered here. Credentials,
//! keychain access, remote adapters, and filesystem capture belong on this
//! side of the boundary; the `review-queue-core` crate remains token-free.

mod commands;
mod copilot_desktop;
mod diagnostics;
mod github_desktop;
mod machines;

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    sync::{Arc, Mutex},
};

use commands::{AppState, initialize_store};
use machines::MachineState;
use review_queue_core::socket;
use review_queue_desktop::{
    connection_health,
    keychain_vault::{Capability, CredentialVault, MacOsKeychainBackend},
};
use tauri::Manager;

fn main() {
    if let Some(exit_code) = run_packaged_keychain_acceptance_from_args() {
        std::process::exit(exit_code);
    }
    tauri::Builder::default()
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(|app| {
            let app_data = app.path().app_data_dir()?;
            fs::create_dir_all(&app_data)?;
            let runtime_dir = app_data.join("runtime");
            fs::create_dir_all(&runtime_dir)?;
            fs::set_permissions(&runtime_dir, fs::Permissions::from_mode(0o700))?;
            let store = Arc::new(Mutex::new(initialize_store(
                app_data.join("review-queue.sqlite3"),
            )?));
            let socket_store = Arc::clone(&store);
            let pr_store = Arc::clone(&store);
            let socket_path = runtime_dir.join("review-queue.sock");
            std::thread::Builder::new()
                .name("review-queue-cli-socket".into())
                .spawn(move || {
                    let pr_add = Arc::new(move |url| {
                        github_desktop::queue_pull_request_from_cli(&pr_store, url)
                    });
                    if let Err(error) =
                        socket::serve_with_pr_handler(socket_path, socket_store, pr_add)
                    {
                        eprintln!("Review Queue local CLI socket stopped: {error:#}");
                    }
                })?;
            app.manage(AppState(store));
            app.manage(copilot_desktop::CopilotDesktopState::new());
            app.manage(MachineState::new(&runtime_dir)?);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::discover_local,
            commands::open_keychain_access,
            commands::preflight_local,
            commands::submit_local,
            commands::list_rounds,
            commands::get_round,
            commands::list_viewed_files,
            commands::set_file_viewed,
            commands::materialize_round_diff,
            commands::materialize_round_file,
            commands::preview_round_reproduction,
            commands::materialize_round_reproduction,
            commands::edit_round_brief,
            commands::get_round_decision,
            commands::request_changes,
            commands::complete_round,
            commands::requeue_round,
            commands::move_round,
            commands::purge_round,
            commands::approve_local,
            commands::approve_remote,
            commands::list_formal_comments,
            commands::create_formal_comment,
            commands::edit_formal_comment,
            commands::delete_formal_comment,
            commands::prepare_feedback_handoff,
            commands::list_agent_routes,
            commands::list_feedback_delivery_history,
            commands::confirm_manual_feedback_submission,
            diagnostics::export_redacted_diagnostics,
            commands::active_conversation,
            commands::current_conversation,
            commands::list_conversation_history,
            commands::list_previous_chats,
            commands::clear_chat,
            commands::queue_ask_turn,
            commands::list_ask_turns,
            commands::begin_ask_turn,
            commands::append_ask_chunk,
            commands::complete_ask_turn,
            commands::cancel_ask_turn,
            commands::fail_ask_turn,
            commands::interrupt_ask_turn,
            copilot_desktop::copilot_capabilities,
            copilot_desktop::copilot_start_session,
            copilot_desktop::copilot_change_option,
            copilot_desktop::copilot_send_prompt,
            copilot_desktop::copilot_poll_prompt,
            copilot_desktop::copilot_cancel_prompt,
            copilot_desktop::copilot_end_session,
            copilot_desktop::copilot_clear_chat,
            machines::add_machine,
            machines::list_machines,
            machines::remove_machine,
            machines::connect_machine,
            machines::disconnect_machine,
            machines::fetch_machine_health,
            machines::fetch_machine_index,
            machines::fetch_machine_item_detail,
            machines::fetch_machine_snapshot,
            machines::materialize_machine_round,
            machines::preview_machine_reproduction,
            machines::materialize_machine_reproduction,
            github_desktop::github_queue_pull_request,
            github_desktop::github_preview_pull_request,
            github_desktop::github_confirm_queue_pull_request,
            github_desktop::github_open_pull_request,
            github_desktop::github_refresh_comments,
            github_desktop::github_check_staleness,
            github_desktop::github_refresh_round,
            github_desktop::github_prepare_publish,
            github_desktop::github_publish,
            github_desktop::github_preview_reproduction,
            github_desktop::github_materialize_reproduction,
            connection_health::connection_status,
            connection_health::retry_connection,
            connection_health::disconnect_capability,
            connection_health::start_device_flow,
            connection_health::cancel_device_flow,
            connection_health::complete_device_flow,
            connection_health::set_public_client_id,
            review_queue_desktop::updater::check_for_update,
            review_queue_desktop::updater::install_update,
            review_queue_desktop::updater::relaunch_after_update,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Review Queue desktop application");
}

/// Runs only from the already-signed packaged binary. The two phases are
/// invoked as separate processes by the acceptance harness to prove that
/// capability-separated Keychain state survives an app restart. The service
/// prefix prevents this path from ever touching production credentials.
fn run_packaged_keychain_acceptance_from_args() -> Option<i32> {
    let mut arguments = std::env::args().skip(1);
    let mode = arguments.next()?;
    if !matches!(
        mode.as_str(),
        "--acceptance-keychain-write" | "--acceptance-keychain-read-delete"
    ) {
        return None;
    }
    let service = match arguments.next() {
        Some(service)
            if service.starts_with("com.reviewqueue.desktop.acceptance.")
                && service.len() <= 180 =>
        {
            service
        }
        _ => {
            eprintln!("invalid disposable acceptance Keychain service");
            return Some(64);
        }
    };
    if arguments.next().is_some() {
        eprintln!("unexpected packaged Keychain acceptance argument");
        return Some(64);
    }
    let vault = CredentialVault::new(MacOsKeychainBackend::with_service(service));
    let result = if mode == "--acceptance-keychain-write" {
        for capability in [
            Capability::PrRead,
            Capability::PrPublish,
            Capability::DevicePending,
        ] {
            let _ = vault.delete(capability);
        }
        vault
            .set(Capability::PrRead, "review-queue-acceptance-read")
            .and_then(|_| vault.set(Capability::PrPublish, "review-queue-acceptance-publish"))
            .and_then(|_| vault.set(Capability::DevicePending, "malformed-acceptance-record"))
    } else {
        let result = (|| {
            if vault.get(Capability::PrRead)?.as_deref() != Some("review-queue-acceptance-read")
                || vault.get(Capability::PrPublish)?.as_deref()
                    != Some("review-queue-acceptance-publish")
            {
                return Err(review_queue_desktop::keychain_vault::VaultError::StorageFailure);
            }
            if vault.get_device_pending()?.is_some() {
                return Err(review_queue_desktop::keychain_vault::VaultError::StorageFailure);
            }
            vault.delete(Capability::PrRead)?;
            if vault.get(Capability::PrRead)?.is_some()
                || vault.get(Capability::PrPublish)?.as_deref()
                    != Some("review-queue-acceptance-publish")
            {
                return Err(review_queue_desktop::keychain_vault::VaultError::StorageFailure);
            }
            vault.delete(Capability::PrPublish)
        })();
        let _ = vault.delete(Capability::PrRead);
        let _ = vault.delete(Capability::PrPublish);
        let _ = vault.delete(Capability::DevicePending);
        result
    };
    match result {
        Ok(()) => {
            println!("packaged Keychain acceptance phase passed");
            Some(0)
        }
        Err(error) => {
            let _ = vault.delete(Capability::PrRead);
            let _ = vault.delete(Capability::PrPublish);
            let _ = vault.delete(Capability::DevicePending);
            eprintln!("packaged Keychain acceptance failed: {error}");
            Some(1)
        }
    }
}
