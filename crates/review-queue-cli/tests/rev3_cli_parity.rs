use std::{
    fs,
    io::{BufRead, BufReader, Write},
    os::unix::{fs::PermissionsExt, net::UnixListener},
    process::Command,
    thread,
};

use serde_json::{Value, json};

fn invoke_json(args: &[&str], responses: Vec<Value>) -> (Vec<Value>, Value) {
    let runtime = tempfile::tempdir().unwrap();
    fs::set_permissions(runtime.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let socket = runtime.path().join("review-queue.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server = thread::spawn(move || {
        let mut requests = Vec::new();
        for response in responses {
            let (mut stream, _) = listener.accept().unwrap();
            let mut line = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut line)
                .unwrap();
            requests.push(serde_json::from_str::<Value>(&line).unwrap());
            stream
                .write_all(serde_json::to_string(&response).unwrap().as_bytes())
                .unwrap();
            stream.write_all(b"\n").unwrap();
            stream.flush().unwrap();
        }
        requests
    });

    let output = Command::new(env!("CARGO_BIN_EXE_review-queue"))
        .args(args)
        .env("REVIEW_QUEUE_SOCKET", &socket)
        .env("GITHUB_TOKEN", "ghp_rev3_cli_must_not_send")
        .env("GH_TOKEN", "github_pat_rev3_cli_must_not_send")
        .env("COPILOT_GITHUB_TOKEN", "copilot_rev3_cli_must_not_send")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let echoed = serde_json::from_str(stdout.trim()).unwrap();
    (server.join().unwrap(), echoed)
}

fn assert_token_free(requests: &[Value]) {
    let encoded = serde_json::to_string(requests).unwrap();
    for forbidden in [
        "ghp_rev3_cli_must_not_send",
        "github_pat_rev3_cli_must_not_send",
        "copilot_rev3_cli_must_not_send",
        "access_token",
        "client_secret",
        "authorization",
        "credential",
    ] {
        assert!(
            !encoded.to_ascii_lowercase().contains(forbidden),
            "CLI socket request leaked forbidden credential material: {forbidden}"
        );
    }
}

fn assert_complete_error(error: &Value, state: &str) {
    assert_eq!(error["status"], "error");
    assert_eq!(error["recovery_state"], state);
    for field in [
        "code",
        "what_happened",
        "why_it_matters",
        "data_safety",
        "next_step",
        "diagnostics_route",
        "cancel_route",
    ] {
        assert!(
            !error[field].as_str().unwrap_or_default().trim().is_empty(),
            "{state}.{field} must be nonempty"
        );
    }
}

#[test]
fn pr_add_json_echoes_existing_id_on_idempotent_rerun_without_credentials() {
    let existing = json!({
        "status": "ok",
        "data": {"outcome": "existing", "round": {"id": "github-round-existing"}}
    });
    for _ in 0..2 {
        let (requests, echoed) = invoke_json(
            &[
                "pr",
                "add",
                "https://github.com/example/repository/pull/17",
                "--json",
            ],
            vec![existing.clone()],
        );
        assert_eq!(requests[0]["type"], "pr_add");
        assert_eq!(echoed, existing["data"]);
        assert_eq!(echoed["round"]["id"], "github-round-existing");
        assert_token_free(&requests);
    }
}

#[test]
fn machine_add_json_echoes_existing_id_on_idempotent_rerun_without_credentials() {
    let existing = json!({
        "status": "ok",
        "data": {
            "machine": {
                "id": "machine-existing",
                "config": {
                    "name": "buildbox",
                    "endpoint": {
                        "kind": "ssh",
                        "target": "review@buildbox",
                        "remote_socket": "/run/review-queue.sock",
                        "adapter": "system_open_ssh"
                    },
                    "source_type": "review_queue_daemon"
                }
            },
            "created": false
        }
    });
    for _ in 0..2 {
        let (requests, echoed) = invoke_json(
            &[
                "machine",
                "add",
                "--name",
                "buildbox",
                "--ssh",
                "review@buildbox",
                "--remote-socket",
                "/run/review-queue.sock",
                "--json",
            ],
            vec![existing.clone()],
        );
        assert_eq!(requests[0]["type"], "add_machine");
        assert_eq!(echoed, existing["data"]);
        assert_eq!(echoed["machine"]["id"], "machine-existing");
        assert_eq!(echoed["created"], false);
        assert_token_free(&requests);
    }
}

#[test]
fn submit_json_echoes_existing_id_on_idempotent_rerun_without_credentials() {
    let preflight = json!({
        "status": "ok",
        "data": {
            "repositories": [],
            "before_fingerprint": "fixture-before",
            "participating_repository_ids": [],
            "origin_route_id": null,
            "preflight_token": "fixture-preflight"
        }
    });
    let existing = json!({
        "status": "ok",
        "data": {"outcome": "existing", "round": {"id": "local-round-existing"}}
    });
    for _ in 0..2 {
        let (requests, echoed) = invoke_json(
            &[
                "submit",
                "/fixture/workspace",
                "--topic",
                "rev3-parity",
                "--title",
                "Rev3 parity",
                "--json",
            ],
            vec![preflight.clone(), existing.clone()],
        );
        assert_eq!(requests[0]["type"], "preflight_capture");
        assert_eq!(requests[1]["type"], "capture_local");
        assert_eq!(
            requests[1]["request"]["preflight_token"],
            "fixture-preflight"
        );
        assert_eq!(echoed, existing["data"]);
        assert_eq!(echoed["round"]["id"], "local-round-existing");
        assert_token_free(&requests);
    }
}

#[test]
fn cli_json_snapshots_unreachable_capability_and_invalid_machine_recovery() {
    let runtime = tempfile::tempdir().unwrap();
    fs::set_permissions(runtime.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let missing_socket = runtime.path().join("missing.sock");
    let unreachable = Command::new(env!("CARGO_BIN_EXE_review-queue"))
        .args(["diagnose", "--json"])
        .env("REVIEW_QUEUE_SOCKET", &missing_socket)
        .output()
        .unwrap();
    assert_eq!(unreachable.status.code(), Some(78));
    let unreachable_json: Value = serde_json::from_slice(&unreachable.stdout).unwrap();
    assert_complete_error(&unreachable_json, "cli_app_or_data_plane_unreachable");

    let socket = runtime.path().join("capability.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut line = String::new();
        BufReader::new(stream.try_clone().unwrap())
            .read_line(&mut line)
            .unwrap();
        let request: Value = serde_json::from_str(&line).unwrap();
        let response = json!({
            "status": "error",
            "error": {
                "code": "pr_read_capability_required",
                "what_happened": "The PR read capability is not connected.",
                "data_safety": "No pull request was fetched and no GitHub write occurred.",
                "next_step": "Connect PR read in Application settings."
            }
        });
        stream
            .write_all(serde_json::to_string(&response).unwrap().as_bytes())
            .unwrap();
        stream.write_all(b"\n").unwrap();
        request
    });
    let capability = Command::new(env!("CARGO_BIN_EXE_review-queue"))
        .args([
            "pr",
            "add",
            "https://github.com/example/repository/pull/17",
            "--json",
        ])
        .env("REVIEW_QUEUE_SOCKET", &socket)
        .output()
        .unwrap();
    assert_eq!(capability.status.code(), Some(77));
    let capability_json: Value = serde_json::from_slice(&capability.stdout).unwrap();
    assert_complete_error(&capability_json, "cli_required_capability_not_connected");
    assert_token_free(&[server.join().unwrap()]);

    let invalid = Command::new(env!("CARGO_BIN_EXE_review-queue"))
        .args([
            "machine",
            "add",
            "--name",
            "bad",
            "--ssh",
            "host",
            "--loopback-socket",
            "/tmp/review-queue.sock",
            "--json",
        ])
        .env("REVIEW_QUEUE_SOCKET", &missing_socket)
        .output()
        .unwrap();
    assert_eq!(invalid.status.code(), Some(64));
    let invalid_json: Value = serde_json::from_slice(&invalid.stdout).unwrap();
    assert_complete_error(&invalid_json, "invalid_machine_config");
}
