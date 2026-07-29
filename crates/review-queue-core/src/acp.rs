//! Side-effect-free originating-agent handoff preparation.
//!
//! ACP route data is useful as a durable record of which agent/session
//! originated a review and whether that session is still accessible. It is
//! never a prompt transport: Review Queue does not queue, interrupt, inject,
//! or type feedback into a running agent session.

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

/// An immutable, copyable prompt plus truthful guidance about where the user
/// can submit it. Constructing this value performs no I/O and changes no
/// delivery state.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct PreparedFeedbackPrompt {
    pub delivery_id: String,
    pub idempotency_key: String,
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
    pub reproduction_required: bool,
    pub guidance: String,
}

/// Creates the only originating-agent feedback handoff supported by core.
/// No socket, terminal, provider, or UI automation is reachable from here.
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
            && matches!(route.status.as_str(), "idle" | "busy")
    });
    let handoff_path = if existing_session_accessible {
        ManualHandoffPath::ExistingSession
    } else {
        ManualHandoffPath::ReproduceAndStartFresh
    };
    let guidance = match handoff_path {
        ManualHandoffPath::ExistingSession => {
            "The originating session is accessible. Copy this immutable prompt, wait until the session is ready, and submit it there manually. Review Queue will never queue, interrupt, or inject it."
        }
        ManualHandoffPath::ReproduceAndStartFresh => {
            "The originating session is closed or unavailable. Preview and confirm reproduction, run the environment setup bundle, start a fresh agent session in that workspace, then submit this immutable prompt manually."
        }
    }
    .to_owned();

    Ok(PreparedFeedbackPrompt {
        delivery_id: delivery.id.clone(),
        idempotency_key: delivery.idempotency_key.clone(),
        prompt: copy_feedback_prompt(delivery),
        route_id: route.map(|route| route.id.clone()),
        agent_id: route.map(|route| route.agent_id.clone()),
        session_id: route.and_then(|route| route.session_id.clone()),
        route_status: route.map(|route| route.status.clone()),
        handoff_path,
        manual_submission_required: true,
        reproduction_required: !existing_session_accessible,
        guidance,
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

    use crate::{Anchor, DeliveryPayload};

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
    fn accessible_idle_or_busy_session_requires_manual_submission() {
        for status in ["idle", "busy"] {
            let prepared = prepare_feedback_prompt(
                &delivery(vec![comment()]),
                Some(&route(status, Some("s"))),
            )
            .unwrap();
            assert_eq!(prepared.handoff_path, ManualHandoffPath::ExistingSession);
            assert!(prepared.manual_submission_required);
            assert!(!prepared.reproduction_required);
            assert!(
                prepared
                    .guidance
                    .contains("never queue, interrupt, or inject")
            );
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
}
