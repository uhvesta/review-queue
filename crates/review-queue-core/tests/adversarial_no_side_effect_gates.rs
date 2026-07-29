use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use chrono::Utc;
use review_queue_core::adapters::{
    AskTurn, AskTurnState, ConversationSessionState, DiscoveredSessionOption,
    GithubPullRequestMetadata, GithubPullRequestState, ImportedComment, ImportedCommentKind,
    SessionOptionKind,
};
use review_queue_core::copilot::{CopilotAdapter, FakeCopilotTransport, LocalConversationAction};
use review_queue_core::diff::materialize_round;
use review_queue_core::github::{FakeGithubTransport, GithubAdapter, LocalCommentDraft};
use review_queue_core::machine::{
    LoopbackFakeTransport, MACHINE_PROTOCOL_VERSION, MachineClient, MachineConfig, MachineCursor,
    MachineEndpoint, MachineHealth, MachineHealthState, MachineItemDetail, MachineItemIndex,
    MachineItemSummary, MachineSnapshot, MachineSnapshotFile, MachineSourceType,
    validate_token_free,
};
use review_queue_core::store::{Store, SubmissionResult};
use review_queue_core::{
    AgentRoute, Collection, RepositorySnapshot, ReviewBrief, Round, Submission, WorkspaceManifest,
    redact_for_diagnostics,
};
use tempfile::TempDir;

struct Fixture {
    _temp: TempDir,
    repo: PathBuf,
    store: Store,
    round: Round,
}

fn run_git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()
        .expect("git must be installed for the immutable-diff fixture");
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("fixture git output is UTF-8")
        .trim()
        .to_owned()
}

fn manifest_for_repo(workspace: &Path, repo: &Path) -> WorkspaceManifest {
    let base_sha = run_git(repo, &["rev-parse", "HEAD~1"]);
    let head_sha = run_git(repo, &["rev-parse", "HEAD"]);
    WorkspaceManifest {
        workspace_id: "workspace-fixture".into(),
        workspace_root: workspace.display().to_string(),
        topic: "no-side-effects".into(),
        repositories: vec![RepositorySnapshot {
            repository_id: "repo".into(),
            root: "repo".into(),
            branch: run_git(repo, &["branch", "--show-current"]),
            base_sha,
            head_sha,
            remote_fingerprint: None,
            object_checksum: "fixture-object-checksum".into(),
            capture_metadata: None,
        }],
        before_fingerprint: "before-fixture".into(),
        after_fingerprint: "after-fixture".into(),
        created_at: Utc::now(),
    }
}

fn brief() -> ReviewBrief {
    ReviewBrief {
        title: "Adversarial no-side-effect gate".into(),
        what: "Exercise read-only product paths.".into(),
        why: "Prevent accidental cross-boundary mutations.".into(),
        approach_alternatives: "Use only public core APIs.".into(),
        testing: "Integration assertions.".into(),
    }
}

fn create_fixture(collection: Collection, persistent_store: bool) -> Fixture {
    let temp = tempfile::tempdir().expect("temporary fixture");
    let repo = temp.path().join("repo");
    fs::create_dir(&repo).expect("create repository");
    run_git(&repo, &["init", "-b", "main"]);
    run_git(&repo, &["config", "user.name", "Review Queue Test"]);
    run_git(
        &repo,
        &["config", "user.email", "review-queue@example.invalid"],
    );
    fs::write(repo.join("review.txt"), "base\n").expect("write base source");
    run_git(&repo, &["add", "review.txt"]);
    run_git(&repo, &["commit", "-m", "base"]);
    fs::write(repo.join("review.txt"), "base\nreviewed change\n").expect("write reviewed source");
    run_git(&repo, &["add", "review.txt"]);
    run_git(&repo, &["commit", "-m", "reviewed change"]);

    let manifest = manifest_for_repo(temp.path(), &repo);
    let mut store = if persistent_store {
        Store::open(temp.path().join("review-queue.sqlite")).expect("open persistent store")
    } else {
        Store::in_memory().expect("open in-memory store")
    };
    let result = store
        .submit(Submission {
            collection,
            topic_identity: format!("{}:fixture", collection.as_str()),
            brief: brief(),
            manifest,
            origin_route: None,
            source_metadata: None,
        })
        .expect("submit fixture round");
    let round = match result {
        SubmissionResult::Created(round) => round,
        other => panic!("fixture must create a round, got {other:?}"),
    };
    Fixture {
        _temp: temp,
        repo,
        store,
        round,
    }
}

fn conversation_options() -> Vec<DiscoveredSessionOption> {
    vec![
        DiscoveredSessionOption {
            key: "model".into(),
            label: "Model".into(),
            kind: SessionOptionKind::Select,
            values: vec!["gpt-fixture".into()],
            selected: Some("gpt-fixture".into()),
            supported: true,
            unavailable_reason: None,
        },
        DiscoveredSessionOption {
            key: "thinking".into(),
            label: "Thinking".into(),
            kind: SessionOptionKind::Select,
            values: vec!["high".into()],
            selected: Some("high".into()),
            supported: true,
            unavailable_reason: None,
        },
        DiscoveredSessionOption {
            key: "context".into(),
            label: "Context".into(),
            kind: SessionOptionKind::Select,
            values: vec!["review".into()],
            selected: Some("review".into()),
            supported: true,
            unavailable_reason: None,
        },
    ]
}

fn queued_turn(conversation_id: &str, id: &str) -> AskTurn {
    AskTurn {
        id: id.into(),
        conversation_id: conversation_id.into(),
        idempotency_key: format!("{id}-idempotency"),
        prompt: "Why is this change safe?".into(),
        anchor: None,
        option_values: BTreeMap::from([
            ("context".into(), "review".into()),
            ("model".into(), "gpt-fixture".into()),
            ("thinking".into(), "high".into()),
        ]),
        state: AskTurnState::Queued,
        created_at: Utc::now(),
        completed_at: None,
        failure_reason: None,
        response_text: String::new(),
    }
}

fn github_metadata() -> GithubPullRequestMetadata {
    GithubPullRequestMetadata {
        host: "github.example.test".into(),
        owner: "octo".into(),
        repository: "review-queue".into(),
        pull_number: 42,
        title: "No-side-effect gates".into(),
        body: "A fixture pull request.".into(),
        base_sha: "base-sha".into(),
        head_sha: "head-sha".into(),
        state: GithubPullRequestState::Open,
        is_draft: false,
        web_url: Some("https://github.example.test/octo/review-queue/pull/42".into()),
    }
}

fn serialized<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_string(value).expect("serialize comparison value")
}

#[test]
fn opening_listing_diff_and_chat_history_are_cross_boundary_read_only() {
    let mut fixture = create_fixture(Collection::Local, false);
    let conversation = fixture
        .store
        .active_conversation(&fixture.round.id, conversation_options())
        .expect("create fixture conversation before the observation baseline");
    let turn = fixture
        .store
        .queue_ask_turn(queued_turn(&conversation.id, "turn-read-only"))
        .expect("durably queue fixture turn");
    fixture
        .store
        .begin_ask_turn(&turn.id)
        .expect("begin fixture stream");
    fixture
        .store
        .append_ask_chunk(&turn.id, "Persisted response.")
        .expect("append fixture transcript");
    fixture
        .store
        .complete_ask_turn(&turn.id)
        .expect("complete fixture turn");

    let before_round = serialized(
        &fixture
            .store
            .round(&fixture.round.id)
            .expect("baseline round"),
    );
    let mut copilot = CopilotAdapter::new(FakeCopilotTransport::healthy());
    let github = GithubAdapter::new(FakeGithubTransport::new(github_metadata()));

    for action in [
        LocalConversationAction::Open,
        LocalConversationAction::Reopen,
        LocalConversationAction::LoadHistory,
    ] {
        let acknowledgement = copilot.local_conversation_action(action);
        assert_eq!(acknowledgement.provider_requests, 0);
    }
    let opened = fixture.store.round(&fixture.round.id).expect("open round");
    let listed = fixture
        .store
        .list(Some(Collection::Local), true)
        .expect("list rounds");
    let conversations = fixture
        .store
        .conversation_history(&fixture.round.id)
        .expect("load chat history");
    let turns = fixture
        .store
        .ask_turns(&conversation.id)
        .expect("load persisted turns");
    let viewed = fixture
        .store
        .viewed_files(&fixture.round.id)
        .expect("load viewed state");
    let diff = materialize_round(&opened).expect("materialize immutable diff");

    assert_eq!(listed.len(), 1);
    assert_eq!(conversations.len(), 1);
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].response_text, "Persisted response.");
    assert!(viewed.is_empty());
    assert_eq!(diff.repositories.len(), 1);
    assert_eq!(diff.repositories[0].files.len(), 1);
    assert_eq!(
        serialized(
            &fixture
                .store
                .round(&fixture.round.id)
                .expect("round after reads")
        ),
        before_round,
        "read paths must not mutate lifecycle, rank, decision-visible round data, or supersession"
    );
    assert_eq!(
        copilot.transport().counters().provider_requests(),
        0,
        "open/reopen/history must not cross the provider boundary"
    );
    assert_eq!(github.transport().total_reads(), 0);
    assert_eq!(github.transport().publish_writes, 0);
    assert!(
        fixture
            .store
            .lifecycle_events(&fixture.round.id)
            .expect("lifecycle audit after reads")
            .is_empty(),
        "opening, listing, diff materialization, and chat history must not create hidden lifecycle entries"
    );
}

#[test]
fn clear_chat_archives_without_prompt_and_preserves_formal_feedback() {
    let mut fixture = create_fixture(Collection::Local, false);
    let old = fixture
        .store
        .active_conversation(&fixture.round.id, conversation_options())
        .expect("active conversation");
    let turn = fixture
        .store
        .queue_ask_turn(queued_turn(&old.id, "turn-before-clear"))
        .expect("queue turn");
    fixture.store.begin_ask_turn(&turn.id).expect("begin turn");
    fixture
        .store
        .append_ask_chunk(&turn.id, "Saved answer.")
        .expect("append answer");
    fixture
        .store
        .complete_ask_turn(&turn.id)
        .expect("complete turn");
    let formal = fixture
        .store
        .create_formal_comment(
            &fixture.round.id,
            "formal-thread",
            "This formal draft must survive Clear chat.",
            None,
        )
        .expect("create formal draft");
    let before_round = serialized(
        &fixture
            .store
            .round(&fixture.round.id)
            .expect("baseline round"),
    );
    let mut copilot = CopilotAdapter::new(FakeCopilotTransport::healthy());

    let acknowledgement = copilot.local_conversation_action(LocalConversationAction::Clear);
    let fresh = fixture
        .store
        .clear_conversation(&fixture.round.id)
        .expect("clear chat");

    assert_eq!(acknowledgement.provider_requests, 0);
    assert_eq!(copilot.transport().counters().prompt_starts, 0);
    assert_eq!(copilot.transport().counters().provider_requests(), 0);
    assert_ne!(fresh.id, old.id);
    assert_eq!(fresh.session_state, ConversationSessionState::CanContinue);
    assert!(fixture.store.ask_turns(&fresh.id).unwrap().is_empty());
    let previous = fixture
        .store
        .previous_conversations(&fixture.round.id)
        .expect("previous chats");
    assert_eq!(previous.len(), 1);
    assert_eq!(previous[0].id, old.id);
    assert_eq!(
        previous[0].session_state,
        ConversationSessionState::HistoryOnly
    );
    assert!(previous[0].archived_at.is_some());
    assert_eq!(
        fixture.store.ask_turns(&old.id).unwrap()[0].response_text,
        "Saved answer."
    );
    assert_eq!(
        fixture
            .store
            .formal_comments(&fixture.round.id)
            .expect("formal drafts after clear"),
        vec![formal]
    );
    assert_eq!(
        serialized(&fixture.store.round(&fixture.round.id).unwrap()),
        before_round
    );

    let rejected = fixture
        .store
        .queue_ask_turn(queued_turn(&old.id, "history-only-turn"))
        .expect_err("history-only chats cannot create a prompt");
    assert_eq!(rejected.error.code, "history_only_conversation");
    assert_eq!(copilot.transport().counters().prompt_starts, 0);
}

#[test]
fn heartbeat_has_no_round_chat_comment_delivery_or_external_side_effect() {
    let mut fixture = create_fixture(Collection::Local, false);
    let route = AgentRoute {
        id: "route-heartbeat".into(),
        adapter_kind: "fake-acp".into(),
        agent_id: "agent-fixture".into(),
        endpoint: Some("127.0.0.1:41999".into()),
        session_id: Some("session-fixture".into()),
        status: "idle".into(),
        last_heartbeat: Utc::now(),
        provenance: None,
    };
    fixture
        .store
        .register_route(&route)
        .expect("register route");
    let conversation = fixture
        .store
        .active_conversation(&fixture.round.id, conversation_options())
        .expect("create chat");
    let comment = fixture
        .store
        .create_formal_comment(
            &fixture.round.id,
            "heartbeat-thread",
            "Heartbeat must not deliver this.",
            None,
        )
        .expect("formal comment");
    let round_before = serialized(&fixture.store.round(&fixture.round.id).unwrap());
    let chat_before = serialized(
        &fixture
            .store
            .conversation_history(&fixture.round.id)
            .unwrap(),
    );
    let comments_before = serialized(&fixture.store.formal_comments(&fixture.round.id).unwrap());
    let copilot = CopilotAdapter::new(FakeCopilotTransport::healthy());
    let github = GithubAdapter::new(FakeGithubTransport::new(github_metadata()));

    fixture
        .store
        .heartbeat(&route.id, "busy")
        .expect("heartbeat registered route");

    let persisted_route = fixture
        .store
        .route(&route.id)
        .expect("route remains publicly inspectable");
    assert_eq!(persisted_route.status, "busy");
    assert!(persisted_route.last_heartbeat >= route.last_heartbeat);
    assert_eq!(fixture.store.routes().expect("list routes").len(), 1);
    assert_eq!(
        serialized(&fixture.store.round(&fixture.round.id).unwrap()),
        round_before
    );
    assert_eq!(
        serialized(
            &fixture
                .store
                .conversation_history(&fixture.round.id)
                .unwrap()
        ),
        chat_before
    );
    assert_eq!(
        serialized(&fixture.store.formal_comments(&fixture.round.id).unwrap()),
        comments_before
    );
    assert_eq!(comment.delivered_revision, None);
    assert_eq!(
        conversation.session_state,
        ConversationSessionState::CanContinue
    );
    assert_eq!(copilot.transport().counters().provider_requests(), 0);
    assert_eq!(github.transport().total_reads(), 0);
    assert_eq!(github.transport().publish_writes, 0);
    assert!(
        fixture
            .store
            .lifecycle_events(&fixture.round.id)
            .expect("lifecycle audit after heartbeat")
            .is_empty(),
        "route registration and heartbeat are liveness, not lifecycle decisions"
    );
}

#[test]
fn purge_removes_app_state_without_touching_git_refs_index_or_worktree() {
    let fixture = create_fixture(Collection::Local, false);
    fs::write(
        fixture.repo.join("review.txt"),
        "base\nreviewed change\npost-review local work\n",
    )
    .expect("make an unstaged source edit");
    fs::write(fixture.repo.join("untracked.txt"), "untracked local work\n")
        .expect("make untracked source");

    let head_before = run_git(&fixture.repo, &["rev-parse", "HEAD"]);
    let refs_before = run_git(&fixture.repo, &["show-ref"]);
    let status_before = run_git(&fixture.repo, &["status", "--porcelain=v1", "-uall"]);
    let tracked_before = fs::read(fixture.repo.join("review.txt")).unwrap();
    let untracked_before = fs::read(fixture.repo.join("untracked.txt")).unwrap();

    fixture
        .store
        .purge(&fixture.round.id)
        .expect("confirmed purge");

    assert_eq!(run_git(&fixture.repo, &["rev-parse", "HEAD"]), head_before);
    assert_eq!(run_git(&fixture.repo, &["show-ref"]), refs_before);
    assert_eq!(
        run_git(&fixture.repo, &["status", "--porcelain=v1", "-uall"]),
        status_before
    );
    assert_eq!(
        fs::read(fixture.repo.join("review.txt")).unwrap(),
        tracked_before
    );
    assert_eq!(
        fs::read(fixture.repo.join("untracked.txt")).unwrap(),
        untracked_before
    );
    assert_eq!(
        fixture
            .store
            .round(&fixture.round.id)
            .unwrap_err()
            .error
            .code,
        "round_not_found"
    );
}

#[test]
fn serialized_database_protocol_and_diagnostics_are_token_scanned() {
    let fixture = create_fixture(Collection::Local, true);
    let round = fixture.store.round(&fixture.round.id).unwrap();
    validate_token_free(&round).expect("representative serialized DB/domain row is token-free");
    validate_token_free(&serde_json::json!({
        "operation": "item_detail",
        "source_item_id": "safe-item"
    }))
    .expect("representative protocol request is token-free");
    redact_for_diagnostics("round=fixture lifecycle=queued")
        .expect("representative diagnostics are token-free");

    let protocol_error = validate_token_free(&serde_json::json!({
        "operation": "item_detail",
        "source_item_id": "ghp_fixture_secret"
    }))
    .expect_err("protocol rejects a token-shaped value");
    assert_eq!(protocol_error.error.code, "machine_credentials_forbidden");
    let diagnostics_error = redact_for_diagnostics("Authorization: Bearer fixture-secret")
        .expect_err("diagnostics reject a token-shaped value");
    assert_eq!(diagnostics_error.error.code, "token_shaped_diagnostic");

    let ingress_error = fixture
        .store
        .create_formal_comment(
            &fixture.round.id,
            "secret-shaped-ingress",
            "accidental ghp_fixture_secret",
            None,
        )
        .expect_err("Store ingress rejects a token-shaped value");
    assert_eq!(ingress_error.error.code, "token_shaped_ingress");
    assert!(
        fixture
            .store
            .formal_comments(&fixture.round.id)
            .expect("comments after rejected ingress")
            .is_empty(),
        "rejected token-shaped input must not persist"
    );
    let export = fixture
        .store
        .export_redacted_artifact(&serde_json::json!({
            "operation": "diagnostics_export",
            "source_item_id": "safe-item"
        }))
        .expect("logical database and protocol projection are token-free");
    assert!(!export.contains_raw_values);
    assert_eq!(export.table_row_counts["rounds"], 1);

    let database_bytes =
        fs::read(fixture._temp.path().join("review-queue.sqlite")).expect("read SQLite artifact");
    for marker in [
        b"ghp_".as_slice(),
        b"github_pat_".as_slice(),
        b"bearer ".as_slice(),
        b"authorization:".as_slice(),
    ] {
        assert!(
            !database_bytes
                .windows(marker.len())
                .any(|window| window.eq_ignore_ascii_case(marker)),
            "SQLite artifact contains forbidden token marker"
        );
    }
}

#[test]
fn explicit_github_comment_refresh_is_pull_only_and_preserves_local_state() {
    let fixture = create_fixture(Collection::Github, false);
    let mut transport = FakeGithubTransport::new(github_metadata());
    transport.comments.push(ImportedComment {
        id: "upstream-comment-1".into(),
        thread_id: "upstream-thread-1".into(),
        body: "Existing upstream review comment.".into(),
        upstream_author: "octocat".into(),
        upstream_created_at: Utc::now(),
        source_url: "https://github.example.test/octo/review-queue/pull/42#discussion_r1".into(),
        kind: ImportedCommentKind::ReviewThreadComment,
        upstream_resolved: Some(false),
        upstream_review_state: None,
        anchor: None,
    });
    let mut github = GithubAdapter::new(transport);
    let queue = github
        .queue_from_url("https://github.example.test/octo/review-queue/pull/42")
        .expect("metadata-only queue payload");
    let local_drafts = vec![LocalCommentDraft {
        id: "local-draft-1".into(),
        thread_id: "upstream-thread-1".into(),
        body: "Unsaved-to-GitHub local formal draft.".into(),
    }];
    let round_before = serialized(&fixture.store.round(&fixture.round.id).unwrap());
    let writes_before = github.transport().publish_writes;

    let refreshed = github
        .refresh_comments(&queue, local_drafts.clone())
        .expect("explicit pull-only refresh");

    assert_eq!(refreshed.local_drafts, local_drafts);
    assert_eq!(refreshed.imported.len(), 1);
    assert_eq!(github.transport().metadata_reads, 1);
    assert_eq!(github.transport().comment_reads, 1);
    assert_eq!(github.transport().file_reads, 0);
    assert_eq!(github.transport().publish_writes, writes_before);
    assert_eq!(
        serialized(&fixture.store.round(&fixture.round.id).unwrap()),
        round_before
    );
}

#[test]
fn connected_machine_transport_runs_only_for_named_fetches() {
    let now = Utc::now();
    let cursor = MachineCursor::new("cursor-1").unwrap();
    let summary = MachineItemSummary {
        source_item_id: "item-1".into(),
        remote_workspace_id: "workspace-1".into(),
        remote_workspace_path: "/srv/review/workspace".into(),
        topic_key: "parser-v2".into(),
        title: "Parser review".into(),
        manifest_hash: "manifest-1".into(),
        snapshot_version: "snapshot-1".into(),
    };
    let detail = MachineItemDetail {
        summary: summary.clone(),
        brief: ReviewBrief {
            title: "Parser review".into(),
            what: "Remote review detail.".into(),
            why: "The parser must remain compatible.".into(),
            approach_alternatives: "Keep the existing parser as the fallback.".into(),
            testing: "Run parser fixtures.".into(),
        },
        repository_count: 1,
        updated_at: now,
        origin_route: None,
    };
    let manifest = WorkspaceManifest {
        workspace_id: "workspace-1".into(),
        workspace_root: "/srv/review/workspace".into(),
        topic: "parser-v2".into(),
        repositories: vec![RepositorySnapshot {
            repository_id: "repo".into(),
            root: "repo".into(),
            branch: "main".into(),
            base_sha: "base-sha".into(),
            head_sha: "head-sha".into(),
            remote_fingerprint: None,
            object_checksum: "checksum".into(),
            capture_metadata: None,
        }],
        before_fingerprint: "before".into(),
        after_fingerprint: "after".into(),
        created_at: now,
    };
    let snapshot = MachineSnapshot {
        source_item_id: summary.source_item_id.clone(),
        snapshot_version: summary.snapshot_version.clone(),
        manifest,
        files: vec![MachineSnapshotFile {
            repository_id: "repo".into(),
            workspace_relative_path: "repo/src/lib.rs".into(),
            status: "modified".into(),
            base_blob_sha: "base-blob".into(),
            head_blob_sha: "head-blob".into(),
            unified_diff: "-old\n+new\n".into(),
            is_binary: false,
            base_content_base64: None,
            head_content_base64: None,
            materialized: None,
        }],
        repository_packs: vec![review_queue_core::machine::MachineRepositoryPack {
            repository_id: "repo".into(),
            head_sha: "head-sha".into(),
            pack_base64: "UEFDSw==".into(),
            shallow_boundary: false,
        }],
    };
    let transport = LoopbackFakeTransport::new(
        MachineHealth {
            protocol_version: MACHINE_PROTOCOL_VERSION,
            daemon_version: "1.0.0".into(),
            state: MachineHealthState::Healthy,
            cursor: cursor.clone(),
        },
        MachineItemIndex {
            cursor,
            items: vec![summary.clone()],
        },
        vec![detail],
        vec![snapshot],
    )
    .expect("valid fake connected machine");
    let mut client = MachineClient::new(
        MachineConfig {
            name: "buildbox".into(),
            endpoint: MachineEndpoint::Loopback {
                socket_path: "/tmp/review-queue-machine-fixture.sock".into(),
            },
            source_type: MachineSourceType::ReviewQueueDaemon,
        },
        transport,
    )
    .expect("construct lazy client");

    assert_eq!(client.transport().counters(), Default::default());
    assert!(client.transport().request_log().is_empty());
    assert!(client.cached_index().is_none());
    assert!(client.cached_detail("item-1").is_none());
    assert!(client.cached_snapshot("item-1", "snapshot-1").is_none());
    assert!(!client.index_freshness(now).cached);
    assert_eq!(client.transport().counters(), Default::default());

    client.fetch_health(now).expect("explicit health fetch");
    assert_eq!(client.transport().counters().health, 1);
    assert_eq!(client.transport().counters().item_index, 0);
    client.fetch_index(now).expect("explicit index fetch");
    assert_eq!(client.transport().counters().item_index, 1);
    assert!(client.cached_index().is_some());
    assert_eq!(client.transport().counters().item_index, 1);
    client
        .fetch_item_detail("item-1", now)
        .expect("explicit detail fetch");
    assert_eq!(client.transport().counters().item_detail, 1);
    assert!(client.cached_detail("item-1").is_some());
    assert_eq!(client.transport().counters().item_detail, 1);
    client
        .fetch_snapshot("item-1", "snapshot-1", now)
        .expect("explicit snapshot fetch");
    assert_eq!(client.transport().counters().snapshot, 1);
    assert!(client.cached_snapshot("item-1", "snapshot-1").is_some());
    assert_eq!(client.transport().counters().snapshot, 1);
    assert_eq!(client.transport().request_log().len(), 4);
}
