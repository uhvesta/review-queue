//! Explicit, token-free ACP feedback delivery.
//!
//! Preparing feedback is side-effect free. Delivery is a separate confirmed
//! desktop operation that sends one immutable, idempotent envelope to the
//! loopback endpoint registered by the originating agent. The CLI socket never
//! exposes this transport.

use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{IpAddr, SocketAddr, TcpStream, ToSocketAddrs},
    time::Duration,
};

use serde::{Deserialize, Serialize};

use crate::{
    AgentRoute, Decision, DomainError, DurableDelivery, FormalComment,
    adapters::validate_token_free_fields,
};

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ManualHandoffPath {
    ExistingSession,
    ReproduceAndStartFresh,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AcpDeliveryPolicy {
    Queue,
    Interrupt,
}

impl AcpDeliveryPolicy {
    pub fn outcome(self) -> &'static str {
        match self {
            Self::Queue => "acp_delivered_queue",
            Self::Interrupt => "acp_delivered_interrupt",
        }
    }
}

/// An immutable, copyable payload plus truthful delivery and fallback state.
/// Constructing this value performs no I/O and changes no delivery state.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct PreparedFeedbackPrompt {
    pub delivery_id: String,
    pub idempotency_key: String,
    pub comment_count: usize,
    pub prompt: String,
    #[serde(default)]
    pub route_id: Option<String>,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub route_status: Option<String>,
    pub handoff_path: ManualHandoffPath,
    pub manual_submission_required: bool,
    pub delivery_available: bool,
    pub busy_policy_required: bool,
    pub reproduction_required: bool,
    pub guidance: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct AcpDeliveryEnvelope {
    pub schema_version: u32,
    pub action: String,
    pub idempotency_key: String,
    pub route_id: String,
    pub agent_id: String,
    pub session_id: String,
    pub policy: AcpDeliveryPolicy,
    pub delivery: DurableDelivery,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct AcpDeliveryReceipt {
    pub delivery_id: String,
    pub idempotency_key: String,
    pub receipt_id: String,
    pub policy: AcpDeliveryPolicy,
}

#[derive(Debug, Deserialize)]
struct AcpWireResponse {
    accepted: bool,
    idempotency_key: String,
    #[serde(default)]
    receipt_id: Option<String>,
}

/// Creates the immutable payload used by both direct ACP delivery and the
/// copy/reproduction fallback.
pub fn prepare_feedback_prompt(
    delivery: &DurableDelivery,
    route: Option<&AgentRoute>,
) -> Result<PreparedFeedbackPrompt, DomainError> {
    validate_delivery(delivery)?;
    if let Some(route) = route {
        validate_route(route)?;
    }

    let existing_session_accessible = route.is_some_and(|route| {
        route
            .session_id
            .as_deref()
            .is_some_and(|session| !session.trim().is_empty())
            && matches!(route.status.as_str(), "active" | "idle" | "busy")
    });
    let delivery_available = existing_session_accessible
        && route.is_some_and(|route| {
            route
                .endpoint
                .as_deref()
                .is_some_and(|endpoint| !endpoint.trim().is_empty())
        });
    let busy_policy_required =
        delivery_available && route.is_some_and(|route| route.status == "busy");
    let handoff_path = if existing_session_accessible {
        ManualHandoffPath::ExistingSession
    } else {
        ManualHandoffPath::ReproduceAndStartFresh
    };
    let guidance = match (delivery_available, busy_policy_required, handoff_path) {
        (true, true, _) => {
            "The originating session is busy. Choose Queue until idle or Interrupt current turn, then review and confirm the exact immutable delivery. Copy remains available as a fallback."
        }
        (true, false, _) => {
            "The originating session is reachable. Review and confirm the exact immutable delivery; Review Queue sends it only after that confirmation."
        }
        (false, _, ManualHandoffPath::ExistingSession) => {
            "The originating session has no reachable ACP endpoint. Copy this immutable prompt and submit it there manually, or reconnect the route and retry."
        }
        (false, _, ManualHandoffPath::ReproduceAndStartFresh) => {
            "The originating session is closed or unavailable. Preview and confirm reproduction, run the environment setup bundle, start a fresh agent session in that workspace, then submit this immutable prompt manually."
        }
    }
    .to_owned();

    Ok(PreparedFeedbackPrompt {
        delivery_id: delivery.id.clone(),
        idempotency_key: delivery.idempotency_key.clone(),
        comment_count: delivery.payload.comments.len(),
        prompt: copy_feedback_prompt(delivery),
        route_id: route.map(|route| route.id.clone()),
        agent_id: route.map(|route| route.agent_id.clone()),
        session_id: route.and_then(|route| route.session_id.clone()),
        route_status: route.map(|route| route.status.clone()),
        handoff_path,
        manual_submission_required: !delivery_available,
        delivery_available,
        busy_policy_required,
        reproduction_required: !existing_session_accessible,
        guidance,
    })
}

/// Sends one newline-delimited, idempotent envelope to the registered
/// loopback ACP endpoint and waits for an acknowledgement carrying the same
/// idempotency key.
pub fn deliver_feedback_tcp(
    delivery: &DurableDelivery,
    route: &AgentRoute,
    policy: AcpDeliveryPolicy,
) -> Result<AcpDeliveryReceipt, DomainError> {
    validate_delivery(delivery)?;
    validate_route(route)?;
    if !matches!(route.status.as_str(), "active" | "idle" | "busy") {
        return Err(actionable(
            "The selected originating agent is disconnected.",
            "The immutable feedback remains saved and nothing was sent.",
            "Reconnect the agent route, or copy the prepared prompt and reproduce the saved round.",
            "acp_route_disconnected",
        ));
    }
    if route.status != "busy" && policy == AcpDeliveryPolicy::Interrupt {
        return Err(actionable(
            "Interrupt is available only while the selected agent is busy.",
            "The immutable feedback remains saved and nothing was sent.",
            "Choose Queue, or refresh the route status and review the delivery again.",
            "acp_interrupt_not_applicable",
        ));
    }
    let session_id = route
        .session_id
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            actionable(
                "The selected agent route has no active session.",
                "The immutable feedback remains saved and nothing was sent.",
                "Reconnect the route, or copy the prompt and reproduce the saved round.",
                "acp_session_unavailable",
            )
        })?;
    let endpoint = route
        .endpoint
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            actionable(
                "The selected agent route has no ACP endpoint.",
                "The immutable feedback remains saved and nothing was sent.",
                "Reconnect the route, or copy the prepared prompt for manual submission.",
                "acp_endpoint_unavailable",
            )
        })?;
    let address = loopback_address(endpoint)?;
    let envelope = AcpDeliveryEnvelope {
        schema_version: 1,
        action: "deliver_feedback".into(),
        idempotency_key: delivery.idempotency_key.clone(),
        route_id: route.id.clone(),
        agent_id: route.agent_id.clone(),
        session_id: session_id.to_owned(),
        policy,
        delivery: delivery.clone(),
    };
    let encoded = serde_json::to_string(&envelope).map_err(|_| {
        actionable(
            "The immutable ACP delivery could not be encoded.",
            "The feedback remains saved and nothing was sent.",
            "Copy the prepared prompt, or retry after reopening Formal feedback.",
            "acp_delivery_encode_failed",
        )
    })?;
    validate_token_free_fields(vec![encoded.as_str()])?;

    let timeout = Duration::from_secs(3);
    let mut stream = TcpStream::connect_timeout(&address, timeout).map_err(|_| {
        actionable(
            "The originating agent ACP endpoint could not be reached.",
            "The immutable feedback remains saved and its revisions are still undelivered.",
            "Reconnect the agent, retry this explicit Send, or copy the prompt and reproduce the saved round.",
            "acp_endpoint_unreachable",
        )
    })?;
    stream.set_read_timeout(Some(timeout)).map_err(|_| {
        actionable(
            "The ACP delivery timeout could not be configured.",
            "The immutable feedback remains saved and nothing was sent.",
            "Copy the prepared prompt or retry the explicit Send.",
            "acp_transport_unavailable",
        )
    })?;
    stream.set_write_timeout(Some(timeout)).map_err(|_| {
        actionable(
            "The ACP delivery timeout could not be configured.",
            "The immutable feedback remains saved and nothing was sent.",
            "Copy the prepared prompt or retry the explicit Send.",
            "acp_transport_unavailable",
        )
    })?;
    stream
        .write_all(encoded.as_bytes())
        .and_then(|_| stream.write_all(b"\n"))
        .and_then(|_| stream.flush())
        .map_err(|_| {
            actionable(
                "The ACP endpoint closed before accepting the feedback.",
                "The immutable delivery remains saved with the same idempotency key; its outcome is not marked delivered.",
                "Inspect the agent session, then retry this explicit Send or copy the prepared prompt.",
                "acp_delivery_interrupted",
            )
        })?;

    let mut response = String::new();
    BufReader::new(stream)
        .take(65_537)
        .read_line(&mut response)
        .map_err(|_| {
            actionable(
                "The ACP endpoint did not return a valid acknowledgement.",
                "The immutable delivery remains saved with the same idempotency key; its outcome is not marked delivered.",
                "Inspect the agent session before retrying, or copy the prepared prompt.",
                "acp_acknowledgement_unavailable",
            )
        })?;
    if response.is_empty() || response.len() > 65_536 {
        return Err(actionable(
            "The ACP endpoint returned an empty or oversized acknowledgement.",
            "The immutable delivery remains saved with the same idempotency key; its outcome is not marked delivered.",
            "Inspect the agent session before retrying, or copy the prepared prompt.",
            "acp_acknowledgement_invalid",
        ));
    }
    validate_token_free_fields(vec![response.as_str()])?;
    let response: AcpWireResponse = serde_json::from_str(&response).map_err(|_| {
        actionable(
            "The ACP endpoint returned an unreadable acknowledgement.",
            "The immutable delivery remains saved with the same idempotency key; its outcome is not marked delivered.",
            "Inspect the agent session before retrying, or copy the prepared prompt.",
            "acp_acknowledgement_invalid",
        )
    })?;
    if response.idempotency_key != delivery.idempotency_key {
        return Err(actionable(
            "The ACP acknowledgement did not match this delivery.",
            "The immutable delivery remains saved and is not marked delivered.",
            "Inspect the agent session and copy the prepared prompt instead of retrying blindly.",
            "acp_acknowledgement_mismatch",
        ));
    }
    if !response.accepted {
        return Err(actionable(
            "The originating agent rejected the formal feedback delivery.",
            "The immutable feedback remains saved and its revisions are still undelivered.",
            "Reconnect or ready the agent, then retry; copy and reproduction remain available.",
            "acp_delivery_rejected",
        ));
    }
    let receipt_id = response
        .receipt_id
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            actionable(
                "The ACP endpoint accepted feedback without a receipt.",
                "The immutable delivery remains saved and is not marked delivered.",
                "Inspect the agent session before retrying, or copy the prepared prompt.",
                "acp_receipt_missing",
            )
        })?;
    validate_token_free_fields(vec![receipt_id.as_str()])?;
    Ok(AcpDeliveryReceipt {
        delivery_id: delivery.id.clone(),
        idempotency_key: delivery.idempotency_key.clone(),
        receipt_id,
        policy,
    })
}

fn loopback_address(endpoint: &str) -> Result<SocketAddr, DomainError> {
    let authority = endpoint.strip_prefix("tcp://").ok_or_else(|| {
        actionable(
            "The originating agent ACP endpoint is not a TCP endpoint.",
            "The immutable feedback remains saved and nothing was sent.",
            "Register a loopback tcp://host:port endpoint, then retry.",
            "acp_endpoint_invalid",
        )
    })?;
    let addresses = authority.to_socket_addrs().map_err(|_| {
        actionable(
            "The originating agent ACP endpoint could not be resolved.",
            "The immutable feedback remains saved and nothing was sent.",
            "Correct the registered loopback endpoint, then retry.",
            "acp_endpoint_invalid",
        )
    })?;
    addresses
        .into_iter()
        .find(|address| match address.ip() {
            IpAddr::V4(ip) => ip.is_loopback(),
            IpAddr::V6(ip) => ip.is_loopback(),
        })
        .ok_or_else(|| {
            actionable(
                "ACP feedback delivery is restricted to a loopback endpoint.",
                "The immutable feedback remains saved and nothing was sent to the network.",
                "Register the originating agent's local loopback endpoint, then retry.",
                "acp_endpoint_not_loopback",
            )
        })
}

fn validate_delivery(delivery: &DurableDelivery) -> Result<(), DomainError> {
    require_nonempty(&delivery.id, "feedback_delivery_id_required")?;
    require_nonempty(
        &delivery.idempotency_key,
        "feedback_delivery_idempotency_required",
    )?;
    require_nonempty(&delivery.payload.round_id, "feedback_round_id_required")?;
    if delivery.payload.comments.is_empty() {
        return Err(actionable(
            "Formal feedback has no undelivered comments.",
            "No prompt was prepared and nothing was sent.",
            "Add or edit at least one formal comment, then prepare the prompt again.",
            "formal_comments_required",
        ));
    }
    let encoded = serde_json::to_string(delivery).map_err(|_| {
        actionable(
            "The durable feedback could not be validated.",
            "No prompt was prepared and nothing was sent.",
            "Recreate the durable feedback and retry.",
            "feedback_validation_failed",
        )
    })?;
    validate_token_free_fields(vec![encoded.as_str()])
}

fn validate_route(route: &AgentRoute) -> Result<(), DomainError> {
    require_nonempty(&route.id, "feedback_route_id_required")?;
    require_nonempty(&route.agent_id, "feedback_agent_id_required")?;
    require_nonempty(&route.status, "feedback_route_status_required")?;
    let encoded = serde_json::to_string(route).map_err(|_| {
        actionable(
            "The originating route metadata could not be validated.",
            "No prompt was prepared and nothing was sent.",
            "Refresh the route metadata and prepare the prompt again.",
            "feedback_route_validation_failed",
        )
    })?;
    validate_token_free_fields(vec![encoded.as_str()])
}

fn copy_feedback_prompt(delivery: &DurableDelivery) -> String {
    let decision = match delivery.payload.decision {
        Decision::Approve => "Approve",
        Decision::RequestChanges => "Request changes",
    };
    let mut prompt = format!(
        "Review round: {}\nFeedback ID: {}\nDecision: {}\n\nFormal feedback:",
        delivery.payload.round_id, delivery.idempotency_key, decision
    );
    for (index, comment) in delivery.payload.comments.iter().enumerate() {
        prompt.push_str(&format!(
            "\n\n{}. {}",
            index + 1,
            format_comment_for_copy(comment)
        ));
    }
    prompt
}

fn format_comment_for_copy(comment: &FormalComment) -> String {
    match &comment.anchor {
        Some(anchor) => format!(
            "{}:{}-{} ({}): {}",
            anchor.workspace_relative_path,
            anchor.start_line,
            anchor.end_line,
            anchor.side,
            comment.body
        ),
        None => comment.body.clone(),
    }
}

fn require_nonempty(value: &str, code: &str) -> Result<(), DomainError> {
    if value.trim().is_empty() {
        return Err(actionable(
            "A required feedback handoff field is empty.",
            "No prompt was prepared and nothing was sent.",
            "Refresh the round and prepare the prompt again.",
            code,
        ));
    }
    Ok(())
}

fn actionable(what: &str, safety: &str, next: &str, code: &str) -> DomainError {
    DomainError::actionable(what, safety, next, code)
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use std::{
        io::{BufRead, BufReader, Write},
        net::TcpListener,
        thread,
    };

    use crate::{
        Anchor, Collection, DeliveryPayload, RepositorySnapshot, ReviewBrief, Submission,
        WorkspaceManifest,
        store::{Store, SubmissionResult},
    };

    use super::*;

    fn delivery(comments: Vec<FormalComment>) -> DurableDelivery {
        DurableDelivery {
            id: "delivery-1".into(),
            idempotency_key: "immutable-feedback-1".into(),
            payload: DeliveryPayload {
                round_id: "round-1".into(),
                decision: Decision::RequestChanges,
                comments,
            },
        }
    }

    fn comment() -> FormalComment {
        FormalComment {
            id: "comment-1".into(),
            thread_id: "thread-1".into(),
            body: "Fix the parser.".into(),
            anchor: Some(Anchor {
                repository_id: "api".into(),
                workspace_relative_path: "src/parser.rs".into(),
                side: "head".into(),
                start_line: 12,
                end_line: 14,
                blob_sha: "abc123".into(),
                selected_code: "parse(input)".into(),
            }),
            revision: 1,
            delivered_revision: None,
        }
    }

    fn route(status: &str, session_id: Option<&str>) -> AgentRoute {
        AgentRoute {
            id: "route-1".into(),
            adapter_kind: "acp".into(),
            agent_id: "parser-agent".into(),
            endpoint: Some("tcp://127.0.0.1:7331".into()),
            session_id: session_id.map(str::to_owned),
            status: status.into(),
            last_heartbeat: Utc::now(),
            provenance: None,
        }
    }

    #[test]
    fn accessible_idle_or_busy_session_prepares_confirmed_delivery() {
        for status in ["idle", "busy"] {
            let prepared = prepare_feedback_prompt(
                &delivery(vec![comment()]),
                Some(&route(status, Some("s"))),
            )
            .unwrap();
            assert_eq!(prepared.handoff_path, ManualHandoffPath::ExistingSession);
            assert!(!prepared.manual_submission_required);
            assert!(prepared.delivery_available);
            assert_eq!(prepared.busy_policy_required, status == "busy");
            assert!(!prepared.reproduction_required);
            assert!(prepared.guidance.contains("confirm"));
            assert!(prepared.prompt.contains("immutable-feedback-1"));
            assert!(prepared.prompt.contains("src/parser.rs:12-14"));
        }
    }

    #[test]
    fn closed_or_missing_session_requires_reproduction_and_fresh_session() {
        for route in [
            None,
            Some(route("disconnected", Some("old"))),
            Some(route("idle", None)),
        ] {
            let prepared =
                prepare_feedback_prompt(&delivery(vec![comment()]), route.as_ref()).unwrap();
            assert_eq!(
                prepared.handoff_path,
                ManualHandoffPath::ReproduceAndStartFresh
            );
            assert!(prepared.reproduction_required);
            assert!(prepared.guidance.contains("start a fresh agent session"));
        }
    }

    #[test]
    fn zero_comment_prompt_is_rejected_without_side_effects() {
        let error = prepare_feedback_prompt(&delivery(vec![]), None).unwrap_err();
        assert_eq!(error.error.code, "formal_comments_required");
        assert!(error.error.data_safety.contains("nothing was sent"));
    }

    #[test]
    fn fake_tcp_agent_receives_each_explicit_revision_set_once_under_new_keys() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let agent = thread::spawn(move || {
            let mut envelopes = Vec::new();
            for index in 1..=2 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut line = String::new();
                BufReader::new(stream.try_clone().unwrap())
                    .read_line(&mut line)
                    .unwrap();
                let envelope: AcpDeliveryEnvelope = serde_json::from_str(&line).unwrap();
                let response = serde_json::json!({
                    "accepted": true,
                    "idempotency_key": envelope.idempotency_key,
                    "receipt_id": format!("fake-agent-receipt-{index}"),
                });
                writeln!(stream, "{response}").unwrap();
                envelopes.push(envelope);
            }
            envelopes
        });

        let route = AgentRoute {
            endpoint: Some(format!("tcp://{address}")),
            session_id: Some("session-fake-agent".into()),
            status: "idle".into(),
            ..route("idle", Some("session-fake-agent"))
        };
        let mut store = Store::in_memory().unwrap();
        store.register_route(&route).unwrap();
        let round = match store
            .submit(Submission {
                collection: Collection::Local,
                topic_identity: "workspace:acp".into(),
                brief: ReviewBrief {
                    title: "ACP delivery".into(),
                    what: "Exercise exact formal feedback.".into(),
                    why: String::new(),
                    approach_alternatives: String::new(),
                    testing: String::new(),
                },
                manifest: WorkspaceManifest {
                    workspace_id: "workspace".into(),
                    workspace_root: "/work".into(),
                    topic: "acp".into(),
                    repositories: vec![RepositorySnapshot {
                        repository_id: "app".into(),
                        root: "app".into(),
                        branch: "main".into(),
                        base_sha: "base".into(),
                        head_sha: "head".into(),
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
            })
            .unwrap()
        {
            SubmissionResult::Created(round) => round,
            _ => panic!("fixture must create one round"),
        };
        store.request_changes(&round.id).unwrap();
        let saved = store
            .create_formal_comment(&round.id, "round", "First revision.", None)
            .unwrap();

        let first = store.prepare_delivery(&round.id).unwrap();
        store.claim_delivery_attempt(&first.id).unwrap();
        let first_receipt = deliver_feedback_tcp(&first, &route, AcpDeliveryPolicy::Queue).unwrap();
        store
            .mark_delivery_acp_delivered(&first.id, first_receipt.policy.outcome())
            .unwrap();

        let edited = store
            .edit_formal_comment(&saved.id, "Second revision only.", None)
            .unwrap();
        assert_eq!(edited.revision, 2);
        assert_eq!(edited.delivered_revision, Some(1));
        let second = store.prepare_delivery(&round.id).unwrap();
        assert_ne!(first.idempotency_key, second.idempotency_key);
        store.claim_delivery_attempt(&second.id).unwrap();
        let second_receipt =
            deliver_feedback_tcp(&second, &route, AcpDeliveryPolicy::Queue).unwrap();
        store
            .mark_delivery_acp_delivered(&second.id, second_receipt.policy.outcome())
            .unwrap();

        let envelopes = agent.join().unwrap();
        assert_eq!(envelopes.len(), 2);
        assert_eq!(envelopes[0].delivery.payload.comments.len(), 1);
        assert_eq!(envelopes[0].delivery.payload.comments[0].revision, 1);
        assert_eq!(
            envelopes[0].delivery.payload.comments[0].body,
            "First revision."
        );
        assert_eq!(envelopes[1].delivery.payload.comments.len(), 1);
        assert_eq!(envelopes[1].delivery.payload.comments[0].revision, 2);
        assert_eq!(
            envelopes[1].delivery.payload.comments[0].body,
            "Second revision only."
        );
        assert_eq!(
            store.formal_comments(&round.id).unwrap()[0].delivered_revision,
            Some(2)
        );
        assert!(
            store
                .delivery_history(&round.id)
                .unwrap()
                .iter()
                .all(|entry| {
                    entry.delivered_at.is_some()
                        && entry.outcome.as_deref() == Some("acp_delivered_queue")
                })
        );
    }
}
