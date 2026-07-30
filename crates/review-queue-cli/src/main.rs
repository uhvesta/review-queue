use std::{
    env,
    path::PathBuf,
    process::ExitCode,
    sync::{Arc, Mutex, OnceLock},
};

use anyhow::Context;
use chrono::Utc;
use review_queue_core::store::Store;
use review_queue_core::{
    AgentRoute, AgentRouteProvenance, ReviewBrief, Round,
    capture::{CaptureRequest, Preflight},
    machine::{
        DEFAULT_REMOTE_SOCKET, MachineConfig, MachineEndpoint, MachineSourceType, SshAdapter,
    },
    reproduction,
    socket::{SocketRequest, SocketResponse, request, serve},
};

const UNREACHABLE: u8 = 78;
const CAPABILITY_REQUIRED: u8 = 77;
const CONFLICT: u8 = 75;
const INVALID_INPUT: u8 = 64;

/// A user-facing failure with stable fields for both terminal and JSON clients.
#[derive(Debug)]
struct CliError {
    code: String,
    what_happened: String,
    data_safety: String,
    next_step: String,
    exit_code: u8,
}

#[derive(Debug, serde::Deserialize)]
struct RecoveryCopyContract {
    schema_version: u32,
    contract: String,
    entries: Vec<RecoveryCopy>,
}

#[derive(Debug, serde::Deserialize)]
struct RecoveryCopy {
    state: String,
    codes: Vec<String>,
    what_happened: String,
    why_it_matters: String,
    data_safety: String,
    next_action: String,
    diagnostics_route: String,
    cancel_route: String,
}

fn recovery_copy_contract() -> &'static RecoveryCopyContract {
    static CONTRACT: OnceLock<RecoveryCopyContract> = OnceLock::new();
    CONTRACT.get_or_init(|| {
        let contract: RecoveryCopyContract = serde_json::from_str(include_str!(
            "../../../frontend/src/recovery-copy-contract.json"
        ))
        .expect("the checked-in Rev3 recovery-copy contract must be valid JSON");
        assert_eq!(contract.schema_version, 3);
        assert_eq!(contract.contract, "rev3_section_8_error_and_recovery");
        for entry in &contract.entries {
            assert!(!entry.state.trim().is_empty());
            assert!(!entry.codes.is_empty());
            assert!(!entry.what_happened.trim().is_empty());
            assert!(!entry.why_it_matters.trim().is_empty());
            assert!(!entry.data_safety.trim().is_empty());
            assert!(!entry.next_action.trim().is_empty());
            assert!(!entry.diagnostics_route.trim().is_empty());
            assert!(!entry.cancel_route.trim().is_empty());
        }
        contract
    })
}

fn recovery_copy_for(code: &str) -> Option<&'static RecoveryCopy> {
    recovery_copy_contract()
        .entries
        .iter()
        .find(|entry| entry.codes.iter().any(|candidate| candidate == code))
}

impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(copy) = recovery_copy_for(&self.code) {
            write!(
                f,
                "{} Why this matters: {} Data safety: {} Next: {} Diagnostics: {} Back: {}",
                copy.what_happened,
                copy.why_it_matters,
                copy.data_safety,
                copy.next_action,
                copy.diagnostics_route,
                copy.cancel_route,
            )?;
            if self.what_happened.trim() != copy.what_happened.trim() {
                write!(f, " Details: {}", self.what_happened)?;
            }
            return Ok(());
        }
        write!(
            f,
            "{} {} Next: {}",
            self.what_happened, self.data_safety, self.next_step
        )
    }
}

impl std::error::Error for CliError {}

fn main() -> ExitCode {
    let args: Vec<_> = env::args().skip(1).collect();
    let json = args.iter().any(|arg| arg == "--json");
    match run(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            if let Some(error) = error.downcast_ref::<CliError>() {
                if json {
                    println!("{}", cli_error_json(error));
                } else {
                    eprintln!("{error}");
                }
                ExitCode::from(error.exit_code)
            } else {
                if json {
                    println!(
                        "{}",
                        serde_json::json!({
                            "status": "error",
                            "code": "invalid_input",
                            "what_happened": error.to_string(),
                            "data_safety": "No source files, commits, or review data were changed.",
                            "next_step": "Run review-queue --help and retry with valid arguments.",
                        })
                    );
                } else {
                    eprintln!("{error:#}");
                }
                ExitCode::from(INVALID_INPUT)
            }
        }
    }
}

fn cli_error_json(error: &CliError) -> serde_json::Value {
    let mut value = serde_json::json!({
        "status": "error",
        "code": error.code,
        "what_happened": error.what_happened,
        "data_safety": error.data_safety,
        "next_step": error.next_step,
    });
    if let Some(copy) = recovery_copy_for(&error.code) {
        let object = value
            .as_object_mut()
            .expect("CLI error JSON is always an object");
        object.insert(
            "what_happened".into(),
            serde_json::Value::String(copy.what_happened.clone()),
        );
        object.insert(
            "why_it_matters".into(),
            serde_json::Value::String(copy.why_it_matters.clone()),
        );
        object.insert(
            "recovery_state".into(),
            serde_json::Value::String(copy.state.clone()),
        );
        object.insert(
            "data_safety".into(),
            serde_json::Value::String(copy.data_safety.clone()),
        );
        object.insert(
            "next_step".into(),
            serde_json::Value::String(copy.next_action.clone()),
        );
        object.insert(
            "diagnostics_route".into(),
            serde_json::Value::String(copy.diagnostics_route.clone()),
        );
        object.insert(
            "cancel_route".into(),
            serde_json::Value::String(copy.cancel_route.clone()),
        );
        if error.what_happened.trim() != copy.what_happened.trim() {
            object.insert(
                "detail".into(),
                serde_json::Value::String(error.what_happened.clone()),
            );
        }
    }
    value
}

fn run(args: Vec<String>) -> anyhow::Result<()> {
    if args.is_empty() || matches!(args[0].as_str(), "--help" | "help") {
        print_usage();
        return Ok(());
    }
    let json = args.iter().any(|arg| arg == "--json");
    match args[0].as_str() {
        "daemon" => {
            let db = required(&args, "--db")?;
            let socket = required(&args, "--socket")?;
            let store = Store::open(db)?;
            eprintln!("Review Queue token-free daemon listening at {socket}");
            serve(socket, Arc::new(Mutex::new(store)))
        }
        "submit" => {
            let mut capture_request = capture_request_from_args(&args)?;
            // Both calls go to the already-running desktop before this process
            // performs any Git operation. The second call owns capture,
            // persistence, index finalization, and rollback.
            let preflight: Preflight = response_data(call(SocketRequest::PreflightCapture {
                request: capture_request.clone(),
            })?)?;
            capture_request.origin_route_id = preflight.origin_route_id;
            capture_request.participating_repository_ids = preflight.participating_repository_ids;
            capture_request.preflight_token = Some(preflight.preflight_token);
            print_response(
                call(SocketRequest::CaptureLocal {
                    request: capture_request,
                })
                .map_err(submit_transport_error)?,
                json,
            )
        }
        "machine" => match args.get(1).map(String::as_str) {
            Some("add") => {
                let config =
                    machine_config_from_args(&args).map_err(invalid_machine_config_error)?;
                print_response(call(SocketRequest::AddMachine { config })?, json)
            }
            Some("ls") => print_response(call(SocketRequest::ListMachines)?, json),
            Some("remove") => {
                let id_or_name = args
                    .get(2)
                    .filter(|value| !value.starts_with("--"))
                    .context("machine remove needs an ID or exact name")?;
                print_response(
                    call(SocketRequest::RemoveMachine {
                        id_or_name: id_or_name.into(),
                    })?,
                    json,
                )
            }
            _ => anyhow::bail!(
                "machine supports: add --name NAME --endpoint SSH_TARGET | ls | remove ID_OR_NAME"
            ),
        },
        "agent" => match args.get(1).map(String::as_str) {
            Some("register") => {
                let route = agent_route_from_args(&args)?;
                print_response(
                    call(SocketRequest::AgentRegister {
                        route: Box::new(route),
                    })?,
                    json,
                )
            }
            Some("heartbeat") => {
                let route_id = required(&args, "--route")?;
                let status = required(&args, "--status")?;
                print_response(
                    call(SocketRequest::AgentHeartbeat {
                        route_id: route_id.into(),
                        status: status.into(),
                    })?,
                    json,
                )
            }
            _ => anyhow::bail!(
                "agent supports: register --route ID --adapter KIND --agent ID [--endpoint URL] [--session ID] [--provenance FILE] [--cwd PATH] [--cmux-workspace ID] [--cmux-surface ID] | heartbeat --route ID --status idle|busy|error"
            ),
        },
        "pr" if args.get(1).map(String::as_str) == Some("add") => {
            let url = args
                .get(2)
                .context("pr add needs a GitHub pull request URL")?;
            print_response(call(SocketRequest::PrAdd { url: url.into() })?, json)
        }
        "reproduce" => reproduce_command(&args, json),
        "diagnose" => print_response(call(SocketRequest::Diagnose)?, json),
        "setup" if args.iter().any(|arg| arg == "--copilot") => setup_copilot_skill(json),
        "pr" => anyhow::bail!("pr supports: add https://github.com/OWNER/REPO/pull/NUMBER"),
        "setup" => anyhow::bail!("setup supports: --copilot"),
        _ => anyhow::bail!("Unknown command '{}'. Run review-queue --help.", args[0]),
    }
}

fn capture_request_from_args(args: &[String]) -> anyhow::Result<CaptureRequest> {
    let workspace = args.get(1).context("submit needs a workspace path")?;
    let topic = required(args, "--topic")?;
    let title = required(args, "--title")?;
    Ok(CaptureRequest {
        workspace_root: PathBuf::from(workspace),
        topic: topic.to_owned(),
        brief: read_brief(args, title)?,
        origin_route_id: optional_nonempty(args, "--route"),
        participating_repository_ids: Vec::new(),
        preflight_token: None,
    })
}

fn agent_route_from_args(args: &[String]) -> anyhow::Result<AgentRoute> {
    let status = optional(args, "--status");
    Ok(AgentRoute {
        id: required(args, "--route")?.into(),
        adapter_kind: required(args, "--adapter")?.into(),
        agent_id: required(args, "--agent")?.into(),
        endpoint: optional_nonempty(args, "--endpoint"),
        session_id: optional_nonempty(args, "--session"),
        status: if status.is_empty() {
            "idle".into()
        } else {
            status
        },
        last_heartbeat: Utc::now(),
        provenance: agent_route_provenance_from_args(args)?,
    })
}

fn agent_route_provenance_from_args(
    args: &[String],
) -> anyhow::Result<Option<Box<AgentRouteProvenance>>> {
    if let Some(path) = optional_nonempty(args, "--provenance") {
        let provenance = serde_json::from_str(
            &std::fs::read_to_string(&path)
                .with_context(|| format!("Could not read provenance file {path}"))?,
        )
        .context("Provenance file must contain AgentRouteProvenance JSON")?;
        return Ok(Some(Box::new(provenance)));
    }

    let provenance = AgentRouteProvenance {
        schema_version: Some(1),
        adapter_version: optional_nonempty(args, "--adapter-version"),
        provider: optional_nonempty(args, "--provider"),
        provider_version: optional_nonempty(args, "--provider-version"),
        machine_id: optional_nonempty(args, "--machine"),
        original_cwd: optional_nonempty(args, "--cwd"),
        cmux_workspace: optional_nonempty(args, "--cmux-workspace"),
        cmux_surface: optional_nonempty(args, "--cmux-surface"),
        reconnect_recipe: optional_nonempty(args, "--reconnect"),
        provider_resume_handle: optional_nonempty(args, "--resume-handle"),
        transcript_reference: optional_nonempty(args, "--transcript"),
        mode: optional_nonempty(args, "--mode"),
        model: optional_nonempty(args, "--model"),
        thinking: optional_nonempty(args, "--thinking"),
        context: optional_nonempty(args, "--context"),
        last_turn: None,
    };
    let has_provenance = provenance
        != AgentRouteProvenance {
            schema_version: Some(1),
            ..AgentRouteProvenance::default()
        };
    Ok(has_provenance.then_some(Box::new(provenance)))
}

fn required<'a>(args: &'a [String], flag: &str) -> anyhow::Result<&'a str> {
    let position = args
        .iter()
        .position(|arg| arg == flag)
        .with_context(|| format!("Missing {flag}"))?;
    args.get(position + 1)
        .map(String::as_str)
        .filter(|value| !value.starts_with("--"))
        .with_context(|| format!("{flag} needs a value"))
}

fn read_brief(args: &[String], title: &str) -> anyhow::Result<ReviewBrief> {
    if let Some(path) = args
        .iter()
        .position(|arg| arg == "--brief")
        .and_then(|i| args.get(i + 1))
    {
        let brief: ReviewBrief = serde_json::from_str(
            &std::fs::read_to_string(path).context("Could not read brief file")?,
        )
        .context("Brief file must contain canonical ReviewBrief JSON")?;
        return Ok(brief);
    }
    Ok(ReviewBrief {
        title: title.into(),
        what: optional(args, "--what"),
        why: optional(args, "--why"),
        approach_alternatives: optional(args, "--approach"),
        testing: optional(args, "--testing"),
    })
}
fn optional(args: &[String], flag: &str) -> String {
    args.iter()
        .position(|arg| arg == flag)
        .and_then(|i| args.get(i + 1))
        .filter(|v| !v.starts_with("--"))
        .cloned()
        .unwrap_or_default()
}

fn optional_nonempty(args: &[String], flag: &str) -> Option<String> {
    let value = optional(args, flag);
    (!value.is_empty()).then_some(value)
}

/// Parses the same token-free `MachineConfig` accepted by the desktop UI.
/// Exactly one endpoint form is required, so an invocation cannot silently
/// turn a local test socket into an SSH connection (or vice versa).
fn machine_config_from_args(args: &[String]) -> anyhow::Result<MachineConfig> {
    let name = required(args, "--name")?.to_owned();
    let ssh = optional_nonempty(args, "--ssh").or_else(|| optional_nonempty(args, "--endpoint"));
    let loopback = optional_nonempty(args, "--loopback-socket");
    let endpoint = match (ssh, loopback) {
        (Some(target), None) => MachineEndpoint::Ssh {
            target,
            remote_socket: optional_nonempty(args, "--remote-socket")
                .unwrap_or_else(|| DEFAULT_REMOTE_SOCKET.to_owned()),
            adapter: SshAdapter::SystemOpenSsh,
        },
        (None, Some(socket_path)) => {
            if optional_nonempty(args, "--remote-socket").is_some() {
                anyhow::bail!("--remote-socket is valid only with --ssh")
            }
            MachineEndpoint::Loopback { socket_path }
        }
        (Some(_), Some(_)) => {
            anyhow::bail!("machine add accepts exactly one of --ssh or --loopback-socket")
        }
        (None, None) => {
            anyhow::bail!("machine add needs --ssh SSH_TARGET or --loopback-socket PATH")
        }
    };
    let config = MachineConfig {
        name,
        endpoint,
        source_type: MachineSourceType::ReviewQueueDaemon,
    };
    config.validate().map_err(anyhow::Error::from)?;
    Ok(config)
}
fn socket_path() -> PathBuf {
    env::var_os("REVIEW_QUEUE_SOCKET")
        .map(PathBuf::from)
        .unwrap_or_else(default_socket_path)
}

fn default_socket_path() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."))
            .join("Library/Application Support/com.reviewqueue.desktop/runtime/review-queue.sock")
    }
    #[cfg(not(target_os = "macos"))]
    {
        env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(".review-queue-runtime"))
            .join("review-queue.sock")
    }
}
fn call(request_: SocketRequest) -> anyhow::Result<SocketResponse> {
    call_at(socket_path(), request_)
}

fn call_at(path: PathBuf, request_: SocketRequest) -> anyhow::Result<SocketResponse> {
    request(path, &request_).map_err(unreachable_error)
}

/// Read-only proof that the app is available.  This must remain ahead of capture(),
/// because capture can create a snapshot commit in one or more repositories.
fn response_data<T: serde::de::DeserializeOwned>(response: SocketResponse) -> anyhow::Result<T> {
    match response {
        SocketResponse::Ok { data } => serde_json::from_value(data).map_err(Into::into),
        SocketResponse::Error { error } => Err(CliError {
            code: error.code,
            what_happened: error.what_happened,
            data_safety: error.data_safety,
            next_step: error.next_step,
            exit_code: INVALID_INPUT,
        }
        .into()),
        SocketResponse::CapturePrepared { .. } => Err(CliError {
            code: "capture_protocol_incomplete".into(),
            what_happened: "The desktop returned an incomplete capture handshake.".into(),
            data_safety:
                "The CLI did not mutate Git; the desktop will roll back an unacknowledged capture."
                    .into(),
            next_step: "Upgrade the CLI and desktop together, then retry.".into(),
            exit_code: CONFLICT,
        }
        .into()),
    }
}

fn reproduce_command(args: &[String], json: bool) -> anyhow::Result<()> {
    let round_id = args.get(1).context("reproduce needs a review round ID")?;
    let destination = required(args, "--destination")?;
    let round: Round = match call(SocketRequest::GetRound {
        id: round_id.into(),
    })? {
        SocketResponse::Ok { data } => {
            serde_json::from_value(data).context("The desktop returned an invalid review round")?
        }
        SocketResponse::Error { error } => {
            return Err(CliError {
                code: error.code,
                what_happened: error.what_happened,
                data_safety: error.data_safety,
                next_step: error.next_step,
                exit_code: INVALID_INPUT,
            }
            .into());
        }
        SocketResponse::CapturePrepared { .. } => {
            anyhow::bail!("The desktop returned a capture handshake for a reproduction request")
        }
    };
    if args.iter().any(|arg| arg == "--confirm") {
        let result = reproduction::materialize(&round.manifest, destination, true)?;
        if json {
            println!("{}", serde_json::to_string(&result)?);
        } else {
            println!("{}", result.command_bundle);
            println!("Reproduced at {}", result.destination);
        }
    } else {
        let preview = reproduction::preview(&round.manifest, destination)?;
        if json {
            println!("{}", serde_json::to_string(&preview)?);
        } else {
            println!("{}", preview.command_bundle);
            println!(
                "Preview only; nothing was created. Re-run with --confirm to materialize at {}.",
                preview.destination
            );
        }
    }
    Ok(())
}

fn setup_copilot_skill(json: bool) -> anyhow::Result<()> {
    let base = env::var_os("HOME")
        .map(PathBuf::from)
        .context("Cannot locate the user profile for Copilot skill installation")?;
    let directory = base.join(".copilot/skills/localreview-submit");
    std::fs::create_dir_all(&directory)
        .with_context(|| format!("Could not create {}", directory.display()))?;
    let path = directory.join("SKILL.md");
    let content = r#"---
name: localreview-submit
description: Capture the current workspace as an immutable local Review Queue round.
---

Collect a concise review title plus What, Why, Approach / Alternatives, and
Testing context. Then invoke the installed token-free CLI:

Register this agent's token-free route once (and heartbeat it as needed):

`review-queue agent register --route <stable-route-id> --adapter copilot-cli --agent <agent-id> --cwd <workspace> --cmux-workspace <workspace-id> --cmux-surface <surface-id> --json`

Then include that registered route in capture:

`review-queue submit <workspace> --route <stable-route-id> --topic <stable-key> --title <title> --what <what> --why <why> --approach <approach> --testing <testing> --json`

Never request or pass credentials. If Review Queue is unavailable, preserve
the user's context and show the CLI's exact recovery action.
"#;
    std::fs::write(&path, content)
        .with_context(|| format!("Could not install {}", path.display()))?;
    let output = serde_json::json!({
        "installed": true,
        "path": path,
        "usage": "/localreview-submit [workspace] [--topic KEY] [--title TEXT]"
    });
    if json {
        println!("{}", serde_json::to_string(&output)?);
    } else {
        println!(
            "Installed /localreview-submit at {}.\nUsage: /localreview-submit [workspace] [--topic KEY] [--title TEXT]",
            path.display()
        );
    }
    Ok(())
}

fn unreachable_error(error: anyhow::Error) -> anyhow::Error {
    CliError {
        code: "app_unreachable".into(),
        what_happened: format!(
            "Review Queue is not running or its local socket is unavailable ({error})."
        ),
        data_safety: "No source files, commits, or review data were changed.".into(),
        next_step: "Launch the Review Queue desktop app, then retry.".into(),
        exit_code: UNREACHABLE,
    }
    .into()
}

/// The desktop owns capture. A disconnect before the two-phase acknowledgement
/// rolls it back; a disconnect after acknowledgement may only make the final
/// response ambiguous, so the recovery action is an idempotent retry.
fn submit_transport_error(error: anyhow::Error) -> anyhow::Error {
    match error.downcast::<CliError>() {
        Ok(error) if error.code == "app_unreachable" => CliError {
            code: error.code,
            what_happened: error.what_happened,
            data_safety: "The CLI never mutated Git. If the desktop accepted the request before the connection failed, its atomic queue record and snapshot commits remain together.".into(),
            next_step: "Retry the same submission. Desktop idempotency will return the existing round instead of creating an orphan or duplicate.".into(),
            exit_code: error.exit_code,
        }
        .into(),
        Ok(error) => error.into(),
        Err(error) => error,
    }
}

fn invalid_machine_config_error(error: anyhow::Error) -> anyhow::Error {
    CliError {
        code: "invalid_machine_config".into(),
        what_happened: format!("The machine configuration is invalid: {error}."),
        data_safety:
            "No machine configuration was saved, no tunnel started, and no credential or review data changed."
                .into(),
        next_step: "Correct the named machine field in the error, then add the machine again."
            .into(),
        exit_code: INVALID_INPUT,
    }
    .into()
}
fn print_response(response: SocketResponse, json: bool) -> anyhow::Result<()> {
    match response {
        SocketResponse::Ok { data } => {
            if json {
                println!("{}", serde_json::to_string(&data)?);
            } else {
                println!("{}", serde_json::to_string_pretty(&data)?);
            }
            Ok(())
        }
        SocketResponse::Error { error } => Err(CliError {
            exit_code: exit_code_for(&error.code),
            code: error.code,
            what_happened: error.what_happened,
            data_safety: error.data_safety,
            next_step: error.next_step,
        }
        .into()),
        SocketResponse::CapturePrepared { .. } => Err(CliError {
            code: "capture_protocol_incomplete".into(),
            what_happened: "The desktop returned an incomplete capture handshake.".into(),
            data_safety: "No successful capture was reported.".into(),
            next_step: "Upgrade the CLI and desktop together, then retry.".into(),
            exit_code: CONFLICT,
        }
        .into()),
    }
}
fn exit_code_for(code: &str) -> u8 {
    if code.ends_with("_required")
        && matches!(
            code,
            "pr_read_required"
                | "pr_read_capability_required"
                | "pr_publish_required"
                | "pr_publish_capability_required"
                | "copilot_connection_required"
                | "capability_required"
        )
    {
        CAPABILITY_REQUIRED
    } else if matches!(
        code,
        "workspace_changed_during_capture"
            | "capture_compensation_required"
            | "github_head_moved"
            | "round_superseded"
            | "machine_cursor_changed"
    ) {
        CONFLICT
    } else {
        INVALID_INPUT
    }
}
fn print_usage() {
    println!(
        "review-queue (token-free CLI)\n\n\
  review-queue daemon --db PATH --socket PATH\n\
  review-queue submit WORKSPACE --topic KEY --title TEXT [--route ID] [--brief brief.json] [--json]\n\
  review-queue pr add https://github.com/OWNER/REPO/pull/NUMBER [--json]\n\
  review-queue machine add --name NAME --ssh SSH_TARGET [--remote-socket PATH] [--json]\n\
  review-queue machine add --name NAME --loopback-socket PATH [--json]\n\
  review-queue machine ls [--json]\n\
  review-queue machine remove ID_OR_NAME [--json]\n\
  review-queue agent register --route ID --adapter KIND --agent ID [--endpoint URL] [--session ID] [--provenance FILE] [--cwd PATH] [--cmux-workspace ID] [--cmux-surface ID] [--json]\n\
  review-queue agent heartbeat --route ID --status idle|busy|error [--json]\n\
  review-queue reproduce ROUND_ID --destination PATH [--confirm] [--json]\n\
  review-queue diagnose [--json]\n\
  review-queue setup --copilot [--json]\n\n\
The CLI never accepts credentials. It sends only token-free read/enqueue requests to the running desktop app's local socket; it cannot deliver feedback or publish."
    );
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        io::{BufRead, BufReader, Write},
        os::unix::net::UnixListener,
        path::Path,
        process::Command,
        thread,
    };

    use super::*;

    fn unique_socket_path(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "review-queue-cli-{label}-{}-{}.sock",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("time after epoch")
                .as_nanos()
        ))
    }

    fn capture_request() -> CaptureRequest {
        CaptureRequest {
            workspace_root: PathBuf::from("/tmp/workspace"),
            topic: "topic".into(),
            brief: ReviewBrief {
                title: "Title".into(),
                what: String::new(),
                why: String::new(),
                approach_alternatives: String::new(),
                testing: String::new(),
            },
            origin_route_id: None,
            participating_repository_ids: Vec::new(),
            preflight_token: None,
        }
    }

    fn git(root: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}");
        String::from_utf8_lossy(&output.stdout).trim_end().into()
    }

    #[test]
    fn exit_codes_distinguish_capability_conflict_and_input_failures() {
        assert_eq!(exit_code_for("pr_read_required"), CAPABILITY_REQUIRED);
        assert_eq!(
            exit_code_for("pr_read_capability_required"),
            CAPABILITY_REQUIRED
        );
        assert_eq!(exit_code_for("workspace_changed_during_capture"), CONFLICT);
        assert_eq!(exit_code_for("topic_required"), INVALID_INPUT);
    }

    #[test]
    fn machine_add_parses_the_full_ui_machine_config_for_ssh_and_loopback() {
        let ssh = machine_config_from_args(&[
            "machine".into(),
            "add".into(),
            "--name".into(),
            "buildbox".into(),
            "--ssh".into(),
            "review@buildbox".into(),
            "--remote-socket".into(),
            "/run/review-queue.sock".into(),
        ])
        .unwrap();
        assert!(
            matches!(ssh.endpoint, MachineEndpoint::Ssh { ref target, ref remote_socket, adapter: SshAdapter::SystemOpenSsh } if target == "review@buildbox" && remote_socket == "/run/review-queue.sock")
        );
        let loopback = machine_config_from_args(&[
            "machine".into(),
            "add".into(),
            "--name".into(),
            "fixture".into(),
            "--loopback-socket".into(),
            "/tmp/fixture.sock".into(),
        ])
        .unwrap();
        assert!(
            matches!(loopback.endpoint, MachineEndpoint::Loopback { ref socket_path } if socket_path == "/tmp/fixture.sock")
        );
        assert!(
            machine_config_from_args(&[
                "machine".into(),
                "add".into(),
                "--name".into(),
                "bad".into(),
                "--ssh".into(),
                "host".into(),
                "--loopback-socket".into(),
                "/tmp/a.sock".into(),
            ])
            .is_err()
        );
    }

    #[test]
    fn submit_and_agent_registration_parse_token_free_route_provenance() {
        let capture = capture_request_from_args(&[
            "submit".into(),
            "/work/review".into(),
            "--topic".into(),
            "parser".into(),
            "--title".into(),
            "Parser review".into(),
            "--route".into(),
            "route-17".into(),
        ])
        .unwrap();
        assert_eq!(capture.origin_route_id.as_deref(), Some("route-17"));

        let route = agent_route_from_args(&[
            "agent".into(),
            "register".into(),
            "--route".into(),
            "route-17".into(),
            "--adapter".into(),
            "acp".into(),
            "--agent".into(),
            "agent-17".into(),
            "--cwd".into(),
            "/work/review".into(),
            "--cmux-workspace".into(),
            "workspace-17".into(),
            "--cmux-surface".into(),
            "surface-17".into(),
            "--provider".into(),
            "copilot-cli".into(),
            "--model".into(),
            "gpt-5.6".into(),
        ])
        .unwrap();
        let provenance = route.provenance.expect("provenance");
        assert_eq!(provenance.original_cwd.as_deref(), Some("/work/review"));
        assert_eq!(provenance.cmux_workspace.as_deref(), Some("workspace-17"));
        assert_eq!(provenance.cmux_surface.as_deref(), Some("surface-17"));
        assert_eq!(provenance.provider.as_deref(), Some("copilot-cli"));
        assert_eq!(provenance.model.as_deref(), Some("gpt-5.6"));
    }

    #[test]
    fn representative_cli_failures_have_stable_complete_json_and_distinct_exit_codes() {
        let cases = [
            (
                CliError {
                    code: "app_unreachable".into(),
                    what_happened: "App unavailable.".into(),
                    data_safety: "Nothing changed.".into(),
                    next_step: "Launch the app.".into(),
                    exit_code: UNREACHABLE,
                },
                UNREACHABLE,
            ),
            (
                CliError {
                    code: "pr_read_required".into(),
                    what_happened: "PR read is disconnected.".into(),
                    data_safety: "No PR was fetched.".into(),
                    next_step: "Connect PR read.".into(),
                    exit_code: CAPABILITY_REQUIRED,
                },
                CAPABILITY_REQUIRED,
            ),
            (
                CliError {
                    code: "workspace_changed_during_capture".into(),
                    what_happened: "Workspace changed.".into(),
                    data_safety: "No torn round exists.".into(),
                    next_step: "Retry capture.".into(),
                    exit_code: CONFLICT,
                },
                CONFLICT,
            ),
            (
                CliError {
                    code: "invalid_machine_config".into(),
                    what_happened: "Machine endpoint is invalid.".into(),
                    data_safety: "Nothing was saved.".into(),
                    next_step: "Correct the endpoint.".into(),
                    exit_code: INVALID_INPUT,
                },
                INVALID_INPUT,
            ),
        ];
        let mut exit_codes = std::collections::BTreeSet::new();
        for (error, expected_exit) in cases {
            let json = cli_error_json(&error);
            for field in ["code", "what_happened", "data_safety", "next_step"] {
                assert!(
                    !json[field].as_str().unwrap_or_default().trim().is_empty(),
                    "{field} must be nonempty"
                );
            }
            assert_eq!(error.exit_code, expected_exit);
            exit_codes.insert(error.exit_code);
        }
        assert_eq!(
            exit_codes.len(),
            4,
            "representative recovery categories use distinct exit codes"
        );
    }

    #[test]
    fn section_8_cli_failures_use_the_shared_complete_recovery_contract() {
        let error = CliError {
            code: "github_publish_rejected".into(),
            what_happened: "GitHub rejected this publish attempt.".into(),
            data_safety: "The formal drafts remain saved.".into(),
            next_step: "This backend copy is intentionally replaced by the contract.".into(),
            exit_code: INVALID_INPUT,
        };
        let json = cli_error_json(&error);
        assert_eq!(json["status"], "error");
        assert_eq!(json["code"], "github_publish_rejected");
        assert_eq!(json["recovery_state"], "publish_rejected");
        assert_eq!(
            json["next_step"],
            "Refresh the pull-request head and reconnect PR publish before preparing one new publish attempt."
        );
        for field in [
            "what_happened",
            "why_it_matters",
            "recovery_state",
            "data_safety",
            "next_step",
            "diagnostics_route",
            "cancel_route",
        ] {
            assert!(
                !json[field].as_str().unwrap_or_default().trim().is_empty(),
                "{field} must be nonempty"
            );
        }
        let terminal = error.to_string();
        assert!(terminal.contains("Why this matters:"));
        assert!(terminal.contains("Diagnostics:"));
        assert!(terminal.contains("Back:"));
    }

    #[test]
    fn rev3_cli_json_snapshot_covers_exactly_all_18_recovery_states() {
        let expected_states = [
            "keychain_unavailable",
            "device_code_expired_or_cancelled",
            "wrong_github_account",
            "copilot_unavailable",
            "model_unavailable",
            "network_unavailable",
            "pr_stale",
            "snapshot_unavailable",
            "acp_busy_or_disconnected",
            "remote_tunnel_failed",
            "publish_rejected",
            "workspace_changed_during_capture",
            "submission_commit_failed",
            "publish_without_decision_unreachable",
            "upstream_comment_refresh_failed",
            "cli_app_or_data_plane_unreachable",
            "cli_required_capability_not_connected",
            "invalid_machine_config",
        ];
        assert_eq!(
            recovery_copy_contract()
                .entries
                .iter()
                .map(|entry| entry.state.as_str())
                .collect::<Vec<_>>(),
            expected_states
        );
        for entry in &recovery_copy_contract().entries {
            let error = CliError {
                code: entry.codes[0].clone(),
                what_happened: entry.what_happened.clone(),
                data_safety: entry.data_safety.clone(),
                next_step: entry.next_action.clone(),
                exit_code: exit_code_for(&entry.codes[0]),
            };
            let json = cli_error_json(&error);
            assert_eq!(json["recovery_state"], entry.state);
            assert_eq!(json["why_it_matters"], entry.why_it_matters);
            assert_eq!(json["data_safety"], entry.data_safety);
            assert_eq!(json["next_step"], entry.next_action);
            assert_eq!(json["diagnostics_route"], entry.diagnostics_route);
            assert_eq!(json["cancel_route"], entry.cancel_route);
        }
    }

    #[test]
    fn preflight_sends_only_a_token_free_capture_preflight_request() {
        let path = unique_socket_path("list");
        let listener = UnixListener::bind(&path).expect("bind test socket");
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept preflight");
            let mut request = String::new();
            BufReader::new(stream.try_clone().expect("clone stream"))
                .read_line(&mut request)
                .expect("read request");
            let parsed: serde_json::Value = serde_json::from_str(&request).expect("parse request");
            assert_eq!(parsed["type"], "preflight_capture");
            assert!(parsed["request"]["preflight_token"].is_null());
            assert_eq!(parsed["request"]["origin_route_id"], "route-safe");
            assert!(parsed["request"].get("origin_route").is_none());
            stream
                .try_clone()
                .expect("clone response stream")
                .write_all(br#"{"status":"ok","data":[]}"#)
                .expect("write response");
        });

        let mut request = capture_request();
        request.origin_route_id = Some("route-safe".into());
        let response = call_at(path.clone(), SocketRequest::PreflightCapture { request })
            .expect("preflight succeeds");
        assert!(matches!(response, SocketResponse::Ok { .. }));
        server.join().expect("server succeeds");
        std::fs::remove_file(path).expect("remove test socket");
    }

    #[test]
    fn unavailable_preflight_is_a_distinct_safe_error() {
        let error = call_at(
            unique_socket_path("missing"),
            SocketRequest::PreflightCapture {
                request: capture_request(),
            },
        )
        .expect_err("must fail");
        let error = error.downcast_ref::<CliError>().expect("structured error");
        assert_eq!(error.code, "app_unreachable");
        assert_eq!(error.exit_code, UNREACHABLE);
        assert!(error.data_safety.contains("commits"));
    }

    #[test]
    fn capture_transport_failure_reports_atomic_retry_semantics() {
        let error = submit_transport_error(unreachable_error(anyhow::anyhow!("gone")));
        let error = error.downcast_ref::<CliError>().expect("structured error");
        assert_eq!(error.code, "app_unreachable");
        assert!(error.data_safety.contains("CLI never mutated Git"));
        assert!(error.next_step.contains("idempotency"));
    }

    #[test]
    fn transport_failure_before_desktop_acceptance_leaves_git_exactly_unchanged() {
        let workspace = tempfile::tempdir().unwrap();
        git(workspace.path(), &["init", "-q"]);
        git(
            workspace.path(),
            &["config", "user.email", "review@example.test"],
        );
        git(workspace.path(), &["config", "user.name", "Review Test"]);
        fs::write(workspace.path().join("tracked.txt"), "initial\n").unwrap();
        git(workspace.path(), &["add", "tracked.txt"]);
        git(workspace.path(), &["commit", "-qm", "initial"]);
        fs::write(workspace.path().join("tracked.txt"), "staged\n").unwrap();
        git(workspace.path(), &["add", "tracked.txt"]);
        fs::write(
            workspace.path().join("tracked.txt"),
            "staged and unstaged\n",
        )
        .unwrap();
        let before = (
            git(workspace.path(), &["rev-parse", "HEAD"]),
            git(
                workspace.path(),
                &["status", "--porcelain=v1", "--untracked-files=all"],
            ),
            fs::read(workspace.path().join(".git/index")).unwrap(),
        );
        let mut request = capture_request();
        request.workspace_root = workspace.path().into();
        let preflight = review_queue_core::capture::preflight(&request).unwrap();
        request.participating_repository_ids = preflight.participating_repository_ids;
        request.preflight_token = Some(preflight.preflight_token);

        call_at(
            unique_socket_path("transport-failure"),
            SocketRequest::CaptureLocal { request },
        )
        .expect_err("missing desktop socket");

        assert_eq!(
            (
                git(workspace.path(), &["rev-parse", "HEAD"]),
                git(
                    workspace.path(),
                    &["status", "--porcelain=v1", "--untracked-files=all"],
                ),
                fs::read(workspace.path().join(".git/index")).unwrap(),
            ),
            before
        );
    }
}
