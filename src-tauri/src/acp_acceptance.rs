//! Opt-in ACP delivery acceptance exercised through the production app binary.
//!
//! The dedicated harness invokes these phases as separate processes. Phase one
//! lets a fake loopback agent durably accept revision 1 and then drops its
//! acknowledgement. Phase two reopens the product database, retries the same
//! immutable key against the fake agent's persisted dedupe state, edits the
//! delivered comment, and explicitly sends only revision 2 under a new key.

use std::{
    fs,
    io::{BufRead, BufReader, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, anyhow, bail};
use chrono::Utc;
use review_queue_core::{
    AgentRoute, Collection, RepositorySnapshot, ReviewBrief, Submission, WorkspaceManifest,
    acp::{AcpDeliveryEnvelope, AcpDeliveryPolicy, prepare_feedback_prompt},
    store::{Store, SubmissionResult},
};
use serde::{Deserialize, Serialize};

use crate::commands::{Confirmation, DeliverFeedbackRequest, deliver_feedback_blocking};

const PHASE_ONE: &str = "--acceptance-acp-phase-one";
const PHASE_TWO: &str = "--acceptance-acp-phase-two";
const ROOT_PREFIX: &str = "review-queue-acp-acceptance.";
const DATABASE_NAME: &str = "review-queue.sqlite3";
const STATE_NAME: &str = "acceptance-state.json";
const AGENT_STATE_NAME: &str = "fake-agent-state.json";

#[derive(Debug, Serialize, Deserialize)]
struct AcceptanceState {
    round_id: String,
    comment_id: String,
    route_id: String,
    first_delivery_id: String,
    first_idempotency_key: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct FakeAgentState {
    accepted: Vec<AcpDeliveryEnvelope>,
}

#[derive(Debug)]
struct FakeAgentRun {
    attempts: Vec<AcpDeliveryEnvelope>,
    state: FakeAgentState,
    duplicates_suppressed: usize,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct PhaseEvidence {
    phase: &'static str,
    database_reopened: bool,
    desktop_confirmations: usize,
    transport_attempts_this_phase: usize,
    unique_agent_accepts_total: usize,
    first_delivery_acknowledged: bool,
    first_attempt_code: Option<String>,
    retry_reused_first_key: bool,
    duplicate_accept_suppressed: bool,
    second_delivery_used_new_key: bool,
    second_delivery_revisions: Vec<i64>,
    delivered_revision: Option<i64>,
    product_database_or_keychain_in_evidence: bool,
}

pub fn run_from_args() -> Option<i32> {
    let mut arguments = std::env::args().skip(1);
    let mode = arguments.next()?;
    if !matches!(mode.as_str(), PHASE_ONE | PHASE_TWO) {
        return None;
    }
    let root = match arguments.next() {
        Some(root) => PathBuf::from(root),
        None => {
            eprintln!("{mode} requires a disposable ACP acceptance directory");
            return Some(64);
        }
    };
    if arguments.next().is_some() {
        eprintln!("unexpected ACP acceptance argument");
        return Some(64);
    }
    let result = match mode.as_str() {
        PHASE_ONE => phase_one(&root),
        PHASE_TWO => phase_two(&root),
        _ => unreachable!(),
    };
    match result {
        Ok(evidence) => {
            println!(
                "{}",
                serde_json::to_string(&evidence).expect("evidence is serializable")
            );
            Some(0)
        }
        Err(error) => {
            eprintln!("packaged ACP acceptance failed: {error:#}");
            Some(1)
        }
    }
}

fn phase_one(root: &Path) -> anyhow::Result<PhaseEvidence> {
    let root = validated_root(root)?;
    let database = root.join(DATABASE_NAME);
    let state_path = root.join(STATE_NAME);
    let agent_state_path = root.join(AGENT_STATE_NAME);
    if database.exists() || state_path.exists() || agent_state_path.exists() {
        bail!("phase one refuses to overwrite existing ACP acceptance state");
    }

    let listener = TcpListener::bind("127.0.0.1:0").context("bind fake ACP agent")?;
    let route = route_for(listener.local_addr()?);
    let agent = spawn_fake_agent(listener, 1, FakeAgentState::default(), true);
    let store = Arc::new(Mutex::new(
        Store::open(&database).context("open ACP acceptance database")?,
    ));
    let (round_id, comment_id, delivery_id, idempotency_key) = {
        let mut locked = store.lock().map_err(|_| anyhow!("lock ACP store"))?;
        locked.register_route(&route)?;
        let round = match locked.submit(submission(&route))? {
            SubmissionResult::Created(round) => round,
            result => bail!("expected ACP round creation, received {result:?}"),
        };
        locked.request_changes(&round.id)?;
        let comment = locked.create_formal_comment(
            &round.id,
            "round",
            "Acceptance feedback revision one.",
            None,
        )?;
        let delivery = locked.prepare_delivery(&round.id)?;
        let prepared = prepare_feedback_prompt(&delivery, Some(&route))?;
        if prepared.comment_count != 1 || !prepared.delivery_available {
            bail!("phase-one delivery was not prepared for one reachable revision");
        }
        (round.id, comment.id, delivery.id, delivery.idempotency_key)
    };

    let failure = deliver_feedback_blocking(
        delivery_request(&round_id, &delivery_id, &route.id, AcpDeliveryPolicy::Queue),
        Arc::clone(&store),
    )
    .expect_err("the fake agent intentionally drops the first acknowledgement");
    if failure.code != "acp_acknowledgement_invalid" {
        bail!(
            "unexpected first delivery failure: expected acp_acknowledgement_invalid, got {}",
            failure.code
        );
    }
    let agent_run = join_fake_agent(agent)?;
    if agent_run.attempts.len() != 1
        || agent_run.state.accepted.len() != 1
        || agent_run.state.accepted[0].idempotency_key != idempotency_key
        || revisions(&agent_run.state.accepted[0]) != [1]
    {
        bail!("fake agent did not accept exactly revision 1 once");
    }
    let history = store
        .lock()
        .map_err(|_| anyhow!("lock ACP store"))?
        .delivery_history(&round_id)?;
    if history.len() != 1
        || history[0].delivered_at.is_some()
        || history[0].outcome.as_deref() != Some("acp_acknowledgement_invalid")
    {
        bail!("ambiguous first attempt was not retained as undelivered");
    }

    fs::write(
        &state_path,
        serde_json::to_vec_pretty(&AcceptanceState {
            round_id,
            comment_id,
            route_id: route.id,
            first_delivery_id: delivery_id,
            first_idempotency_key: idempotency_key,
        })?,
    )
    .context("write ACP acceptance state")?;
    fs::write(
        &agent_state_path,
        serde_json::to_vec_pretty(&agent_run.state)?,
    )
    .context("write fake agent dedupe state")?;

    Ok(PhaseEvidence {
        phase: "phase_one",
        database_reopened: false,
        desktop_confirmations: 1,
        transport_attempts_this_phase: 1,
        unique_agent_accepts_total: 1,
        first_delivery_acknowledged: false,
        first_attempt_code: Some(failure.code),
        retry_reused_first_key: false,
        duplicate_accept_suppressed: false,
        second_delivery_used_new_key: false,
        second_delivery_revisions: Vec::new(),
        delivered_revision: None,
        product_database_or_keychain_in_evidence: false,
    })
}

fn phase_two(root: &Path) -> anyhow::Result<PhaseEvidence> {
    let root = validated_root(root)?;
    let database = root.join(DATABASE_NAME);
    let state: AcceptanceState =
        serde_json::from_slice(&fs::read(root.join(STATE_NAME)).context("read ACP phase state")?)
            .context("decode ACP phase state")?;
    let fake_state: FakeAgentState = serde_json::from_slice(
        &fs::read(root.join(AGENT_STATE_NAME)).context("read fake agent state")?,
    )
    .context("decode fake agent state")?;
    if fake_state.accepted.len() != 1 {
        bail!("phase two requires exactly one previously accepted ACP delivery");
    }

    let listener = TcpListener::bind("127.0.0.1:0").context("bind restarted fake ACP agent")?;
    let route = route_for(listener.local_addr()?);
    if route.id != state.route_id {
        bail!("restarted fake route identity changed");
    }
    let agent = spawn_fake_agent(listener, 2, fake_state, false);
    let store = Arc::new(Mutex::new(
        Store::open(&database).context("reopen ACP acceptance database")?,
    ));
    {
        let mut locked = store.lock().map_err(|_| anyhow!("lock ACP store"))?;
        locked.register_route(&route)?;
        let pending = locked
            .prepare_delivery(&state.round_id)
            .context("reuse first immutable delivery")?;
        if pending.id != state.first_delivery_id
            || pending.idempotency_key != state.first_idempotency_key
        {
            bail!("restart did not preserve the first delivery and idempotency key");
        }
    }

    let retry_receipt = deliver_feedback_blocking(
        delivery_request(
            &state.round_id,
            &state.first_delivery_id,
            &state.route_id,
            AcpDeliveryPolicy::Queue,
        ),
        Arc::clone(&store),
    )
    .map_err(command_error)?;
    if retry_receipt.idempotency_key != state.first_idempotency_key {
        bail!("retry acknowledgement used a different idempotency key");
    }

    let (second_delivery_id, second_idempotency_key) = {
        let mut locked = store.lock().map_err(|_| anyhow!("lock ACP store"))?;
        let edited = locked.edit_formal_comment(
            &state.comment_id,
            "Acceptance feedback revision two.",
            None,
        )?;
        if edited.revision != 2 || edited.delivered_revision != Some(1) {
            bail!("editing the acknowledged comment did not create revision 2");
        }
        let second = locked.prepare_delivery(&state.round_id)?;
        if second.idempotency_key == state.first_idempotency_key
            || second.payload.comments.len() != 1
            || second.payload.comments[0].revision != 2
        {
            bail!("second delivery was not a new key carrying only revision 2");
        }
        (second.id, second.idempotency_key)
    };
    let second_receipt = deliver_feedback_blocking(
        delivery_request(
            &state.round_id,
            &second_delivery_id,
            &state.route_id,
            AcpDeliveryPolicy::Queue,
        ),
        Arc::clone(&store),
    )
    .map_err(command_error)?;
    if second_receipt.idempotency_key != second_idempotency_key {
        bail!("second acknowledgement did not match its new idempotency key");
    }

    let agent_run = join_fake_agent(agent)?;
    if agent_run.attempts.len() != 2
        || agent_run.duplicates_suppressed != 1
        || agent_run.state.accepted.len() != 2
        || agent_run.attempts[0].idempotency_key != state.first_idempotency_key
        || agent_run.attempts[1].idempotency_key != second_idempotency_key
        || revisions(&agent_run.attempts[0]) != [1]
        || revisions(&agent_run.attempts[1]) != [2]
    {
        bail!("fake agent retry/new-revision transcript violated ACP idempotency");
    }
    let locked = store.lock().map_err(|_| anyhow!("lock ACP store"))?;
    let comments = locked.formal_comments(&state.round_id)?;
    let history = locked.delivery_history(&state.round_id)?;
    if comments.len() != 1
        || comments[0].revision != 2
        || comments[0].delivered_revision != Some(2)
        || history.len() != 2
        || history.iter().any(|entry| {
            entry.delivered_at.is_none() || entry.outcome.as_deref() != Some("acp_delivered_queue")
        })
    {
        bail!("desktop delivery outcomes did not durably advance through revision 2");
    }

    Ok(PhaseEvidence {
        phase: "phase_two",
        database_reopened: true,
        desktop_confirmations: 2,
        transport_attempts_this_phase: 2,
        unique_agent_accepts_total: agent_run.state.accepted.len(),
        first_delivery_acknowledged: true,
        first_attempt_code: None,
        retry_reused_first_key: true,
        duplicate_accept_suppressed: true,
        second_delivery_used_new_key: true,
        second_delivery_revisions: revisions(&agent_run.attempts[1]),
        delivered_revision: comments[0].delivered_revision,
        product_database_or_keychain_in_evidence: false,
    })
}

fn route_for(address: std::net::SocketAddr) -> AgentRoute {
    AgentRoute {
        id: "route-acp-acceptance".into(),
        adapter_kind: "fake_acp_acceptance".into(),
        agent_id: "fake-agent".into(),
        endpoint: Some(format!("tcp://{address}")),
        session_id: Some("session-acp-acceptance".into()),
        status: "idle".into(),
        last_heartbeat: Utc::now(),
        provenance: None,
    }
}

fn submission(route: &AgentRoute) -> Submission {
    Submission {
        collection: Collection::Github,
        topic_identity: "acceptance:acp-delivery".into(),
        brief: ReviewBrief {
            title: "ACP packaged acceptance".into(),
            what: "Exercise explicit formal feedback delivery.".into(),
            why: "Release evidence must cover the production desktop path.".into(),
            approach_alternatives: "Use disposable restart-separated state.".into(),
            testing: "Drop acknowledgement, retry, edit, and deliver revision 2.".into(),
        },
        manifest: WorkspaceManifest {
            workspace_id: "acp-acceptance".into(),
            workspace_root: "/disposable/acp-acceptance".into(),
            topic: "acp-delivery".into(),
            repositories: vec![RepositorySnapshot {
                repository_id: "fixture".into(),
                root: "fixture".into(),
                branch: "main".into(),
                base_sha: "acceptance-base".into(),
                head_sha: "acceptance-head".into(),
                remote_fingerprint: None,
                object_checksum: String::new(),
                capture_metadata: None,
            }],
            before_fingerprint: "before".into(),
            after_fingerprint: "after".into(),
            created_at: Utc::now(),
        },
        origin_route: Some(route.clone()),
        source_metadata: None,
        source_adapter: None,
    }
}

fn delivery_request(
    round_id: &str,
    delivery_id: &str,
    route_id: &str,
    policy: AcpDeliveryPolicy,
) -> DeliverFeedbackRequest {
    DeliverFeedbackRequest {
        round_id: round_id.into(),
        delivery_id: delivery_id.into(),
        route_id: route_id.into(),
        policy,
        confirmation: Confirmation {
            confirmed: true,
            token: format!("acp-deliver:{delivery_id}"),
        },
    }
}

fn spawn_fake_agent(
    listener: TcpListener,
    expected_attempts: usize,
    mut state: FakeAgentState,
    drop_first_acknowledgement: bool,
) -> thread::JoinHandle<anyhow::Result<FakeAgentRun>> {
    thread::spawn(move || {
        listener.set_nonblocking(true)?;
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut attempts = Vec::with_capacity(expected_attempts);
        let mut duplicates_suppressed = 0;
        while attempts.len() < expected_attempts {
            let (mut stream, _) = match listener.accept() {
                Ok(connection) => connection,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        bail!("timed out waiting for fake ACP delivery");
                    }
                    thread::sleep(Duration::from_millis(10));
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            let envelope = read_envelope(&mut stream)?;
            let existing_index = state
                .accepted
                .iter()
                .position(|accepted| accepted.idempotency_key == envelope.idempotency_key);
            let receipt_index = if let Some(index) = existing_index {
                duplicates_suppressed += 1;
                index
            } else {
                state.accepted.push(envelope.clone());
                state.accepted.len() - 1
            };
            attempts.push(envelope.clone());
            if drop_first_acknowledgement && attempts.len() == 1 {
                continue;
            }
            let response = serde_json::json!({
                "accepted": true,
                "idempotency_key": envelope.idempotency_key,
                "receipt_id": format!("fake-acp-receipt-{}", receipt_index + 1),
            });
            writeln!(stream, "{response}")?;
        }
        Ok(FakeAgentRun {
            attempts,
            state,
            duplicates_suppressed,
        })
    })
}

fn read_envelope(stream: &mut TcpStream) -> anyhow::Result<AcpDeliveryEnvelope> {
    stream.set_read_timeout(Some(Duration::from_secs(3)))?;
    let mut line = String::new();
    BufReader::new(stream.try_clone()?)
        .read_line(&mut line)
        .context("read fake ACP delivery")?;
    if line.is_empty() {
        bail!("fake ACP agent received an empty delivery");
    }
    serde_json::from_str(&line).context("decode fake ACP delivery")
}

fn join_fake_agent(
    agent: thread::JoinHandle<anyhow::Result<FakeAgentRun>>,
) -> anyhow::Result<FakeAgentRun> {
    agent
        .join()
        .map_err(|_| anyhow!("fake ACP agent thread panicked"))?
}

fn revisions(envelope: &AcpDeliveryEnvelope) -> Vec<i64> {
    envelope
        .delivery
        .payload
        .comments
        .iter()
        .map(|comment| comment.revision)
        .collect()
}

fn validated_root(root: &Path) -> anyhow::Result<PathBuf> {
    let canonical = root
        .canonicalize()
        .context("ACP acceptance directory must already exist")?;
    let name = canonical
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow!("ACP acceptance directory has no valid basename"))?;
    if !name.starts_with(ROOT_PREFIX) {
        bail!("refusing ACP acceptance outside a {ROOT_PREFIX}* directory");
    }
    Ok(canonical)
}

fn command_error(error: crate::commands::CommandError) -> anyhow::Error {
    anyhow!(
        "{}: {} Next: {}",
        error.code,
        error.message,
        error.next_step
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packaged_acp_phases_reopen_retry_dedupe_and_deliver_revision_two() {
        let temporary = tempfile::Builder::new()
            .prefix(ROOT_PREFIX)
            .tempdir()
            .unwrap();
        let first = phase_one(temporary.path()).unwrap();
        assert_eq!(
            first.first_attempt_code.as_deref(),
            Some("acp_acknowledgement_invalid")
        );
        assert_eq!(first.unique_agent_accepts_total, 1);
        assert!(!first.first_delivery_acknowledged);

        let second = phase_two(temporary.path()).unwrap();
        assert!(second.database_reopened);
        assert!(second.retry_reused_first_key);
        assert!(second.duplicate_accept_suppressed);
        assert!(second.second_delivery_used_new_key);
        assert_eq!(second.second_delivery_revisions, [2]);
        assert_eq!(second.delivered_revision, Some(2));
        assert_eq!(second.unique_agent_accepts_total, 2);
        assert!(!second.product_database_or_keychain_in_evidence);
    }
}
