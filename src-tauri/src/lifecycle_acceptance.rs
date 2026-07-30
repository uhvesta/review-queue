//! Opt-in lifecycle acceptance exercised through the production app binary.
//!
//! The packaged smoke script invokes these phases as separate processes. All
//! fixture repositories and SQLite state live below a disposable, explicitly
//! named directory, while the assertions prove lifecycle-only operations do
//! not touch the captured source commits or files.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, anyhow, bail};
use review_queue_core::{
    Collection, Lifecycle, ReviewBrief, Submission,
    capture::CaptureRequest,
    store::{LifecycleEventKind, Store, SubmissionResult},
};
use serde::{Deserialize, Serialize};

use crate::commands::{
    Confirmation, approve_local_with_confirmation, purge_round_with_confirmation,
};

const PHASE_ONE: &str = "--acceptance-lifecycle-phase-one";
const PHASE_TWO: &str = "--acceptance-lifecycle-phase-two";
const ROOT_PREFIX: &str = "review-queue-lifecycle-acceptance.";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct RepositoryState {
    head: String,
    tree: String,
    status: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct AcceptanceState {
    lifecycle_round_id: String,
    delete_round_id: String,
    approve_round_id: String,
    resubmit_old_round_id: String,
    resubmit_new_round_id: String,
    expected_active_ranks: BTreeMap<String, i64>,
    repositories: BTreeMap<String, RepositoryState>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PhaseEvidence {
    phase: &'static str,
    database_reopened: bool,
    request_complete_requeue_events: Vec<String>,
    delete_cancel_code: Option<String>,
    approve_cancel_code: Option<String>,
    deleted_round_purged: bool,
    approved_round_purged: bool,
    superseded_round_retained: bool,
    active_ranks: BTreeMap<String, i64>,
    source_commits_and_files_unchanged: bool,
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
            eprintln!("{mode} requires a disposable acceptance directory");
            return Some(64);
        }
    };
    if arguments.next().is_some() {
        eprintln!("unexpected lifecycle acceptance argument");
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
            eprintln!("packaged lifecycle acceptance failed: {error:#}");
            Some(1)
        }
    }
}

fn phase_one(root: &Path) -> anyhow::Result<PhaseEvidence> {
    let root = validated_root(root)?;
    let database = root.join("review-queue.sqlite3");
    let state_path = root.join("acceptance-state.json");
    let workspace = root.join("workspace");
    if database.exists() || state_path.exists() || workspace.exists() {
        bail!("phase one refuses to overwrite existing acceptance state");
    }
    fs::create_dir(&workspace).context("create lifecycle fixture workspace")?;
    let repositories = [workspace.join("repo-a"), workspace.join("repo-b")];
    for (index, repository) in repositories.iter().enumerate() {
        create_repository(repository, index)?;
    }

    let mut store = Store::open(&database).context("open lifecycle acceptance database")?;
    let first = submit_workspace(&mut store, &workspace, "resubmit", "Initial snapshot")?;
    let resubmit_old_round_id = match first {
        SubmissionResult::Created(round) => round.id,
        result => bail!("expected initial round creation, received {result:?}"),
    };

    let old_round = store.round(&resubmit_old_round_id)?;
    let lifecycle_round =
        create_peer_round(&mut store, &old_round, "lifecycle", "Lifecycle controls")?;
    let delete_round = create_peer_round(&mut store, &old_round, "delete", "Delete confirmation")?;
    let approve_round =
        create_peer_round(&mut store, &old_round, "approve", "Approve confirmation")?;
    let rank_peer = create_peer_round(&mut store, &old_round, "rank", "Rank persistence")?;

    for (index, repository) in repositories.iter().enumerate() {
        fs::write(
            repository.join("review.txt"),
            format!("resubmitted content for repository {index}\n"),
        )
        .context("write resubmission fixture")?;
    }
    let second = submit_workspace(&mut store, &workspace, "resubmit", "Updated snapshot")?;
    let resubmit_new_round_id = match second {
        SubmissionResult::Superseded { old_id, round } => {
            if old_id != resubmit_old_round_id {
                bail!("resubmission superseded an unexpected round");
            }
            round.id
        }
        result => bail!("expected resubmission to supersede, received {result:?}"),
    };
    let source_baseline = repository_states(&repositories)?;

    store.request_changes(&lifecycle_round.id)?;
    store.complete(&lifecycle_round.id)?;
    store.requeue(&lifecycle_round.id)?;
    store.move_rank(&lifecycle_round.id, i64::MAX)?;

    let delete_cancel = purge_round_with_confirmation(
        &store,
        &delete_round.id,
        &Confirmation {
            confirmed: false,
            token: format!("purge:{}", delete_round.id),
        },
    )
    .expect_err("delete cancellation must not purge");
    let approve_cancel = approve_local_with_confirmation(
        &store,
        &approve_round.id,
        &Confirmation {
            confirmed: false,
            token: format!("approve-local:{}", approve_round.id),
        },
    )
    .expect_err("approve cancellation must not purge");
    store.round(&delete_round.id)?;
    store.round(&approve_round.id)?;

    let expected_active_ranks = active_rank_map(&store)?;
    if !expected_active_ranks.contains_key(&rank_peer.id)
        || !expected_active_ranks.contains_key(&resubmit_new_round_id)
    {
        bail!("rank acceptance peers were not retained");
    }
    if repository_states(&repositories)? != source_baseline {
        bail!("phase-one lifecycle actions changed source repositories");
    }
    let state = AcceptanceState {
        lifecycle_round_id: lifecycle_round.id.clone(),
        delete_round_id: delete_round.id,
        approve_round_id: approve_round.id,
        resubmit_old_round_id,
        resubmit_new_round_id,
        expected_active_ranks: expected_active_ranks.clone(),
        repositories: source_baseline,
    };
    fs::write(&state_path, serde_json::to_vec_pretty(&state)?)
        .context("write lifecycle phase state")?;

    Ok(PhaseEvidence {
        phase: "phase_one",
        database_reopened: false,
        request_complete_requeue_events: lifecycle_event_names(&store, &lifecycle_round.id)?,
        delete_cancel_code: Some(delete_cancel.code),
        approve_cancel_code: Some(approve_cancel.code),
        deleted_round_purged: false,
        approved_round_purged: false,
        superseded_round_retained: store.round(&state.resubmit_old_round_id).is_ok(),
        active_ranks: expected_active_ranks,
        source_commits_and_files_unchanged: true,
    })
}

fn phase_two(root: &Path) -> anyhow::Result<PhaseEvidence> {
    let root = validated_root(root)?;
    let database = root.join("review-queue.sqlite3");
    let state_path = root.join("acceptance-state.json");
    let state: AcceptanceState =
        serde_json::from_slice(&fs::read(&state_path).context("read lifecycle phase state")?)
            .context("decode lifecycle phase state")?;
    let store = Store::open(&database).context("reopen lifecycle acceptance database")?;

    let active_ranks = active_rank_map(&store)?;
    if active_ranks != state.expected_active_ranks {
        bail!(
            "active queue ranks changed across restart: expected {:?}, got {:?}",
            state.expected_active_ranks,
            active_ranks
        );
    }
    let lifecycle = store.round(&state.lifecycle_round_id)?;
    if lifecycle.lifecycle != Lifecycle::Queued {
        bail!("requeued lifecycle round did not persist as queued");
    }
    let old = store.round(&state.resubmit_old_round_id)?;
    if old.lifecycle != Lifecycle::Completed
        || old.superseded_by.as_deref() != Some(state.resubmit_new_round_id.as_str())
    {
        bail!("resubmission supersession did not survive restart");
    }

    let current_sources =
        repository_states(&[root.join("workspace/repo-a"), root.join("workspace/repo-b")])?;
    if current_sources != state.repositories {
        bail!("source repositories changed between lifecycle phases");
    }

    purge_round_with_confirmation(
        &store,
        &state.delete_round_id,
        &Confirmation {
            confirmed: true,
            token: format!("purge:{}", state.delete_round_id),
        },
    )
    .map_err(command_error)?;
    approve_local_with_confirmation(
        &store,
        &state.approve_round_id,
        &Confirmation {
            confirmed: true,
            token: format!("approve-local:{}", state.approve_round_id),
        },
    )
    .map_err(command_error)?;
    let deleted_round_purged = store.round(&state.delete_round_id).is_err();
    let approved_round_purged = store.round(&state.approve_round_id).is_err();
    if !deleted_round_purged || !approved_round_purged {
        bail!("confirmed delete/approve did not purge app-owned rounds");
    }
    let delete_events = lifecycle_event_names(&store, &state.delete_round_id)?;
    let approve_events = lifecycle_event_names(&store, &state.approve_round_id)?;
    if delete_events != ["purge"] || approve_events != ["approve_local"] {
        bail!("terminal lifecycle audit events were not retained");
    }
    if repository_states(&[root.join("workspace/repo-a"), root.join("workspace/repo-b")])?
        != state.repositories
    {
        bail!("terminal lifecycle actions changed source repositories");
    }

    Ok(PhaseEvidence {
        phase: "phase_two",
        database_reopened: true,
        request_complete_requeue_events: lifecycle_event_names(&store, &state.lifecycle_round_id)?,
        delete_cancel_code: None,
        approve_cancel_code: None,
        deleted_round_purged,
        approved_round_purged,
        superseded_round_retained: store.round(&state.resubmit_old_round_id).is_ok(),
        active_ranks,
        source_commits_and_files_unchanged: true,
    })
}

fn validated_root(root: &Path) -> anyhow::Result<PathBuf> {
    let canonical = root
        .canonicalize()
        .context("acceptance directory must already exist")?;
    let name = canonical
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow!("acceptance directory has no valid basename"))?;
    if !name.starts_with(ROOT_PREFIX) {
        bail!("refusing lifecycle acceptance outside a {ROOT_PREFIX}* directory");
    }
    Ok(canonical)
}

fn create_repository(repository: &Path, index: usize) -> anyhow::Result<()> {
    fs::create_dir(repository).context("create fixture repository")?;
    git(repository, &["init"])?;
    git(repository, &["config", "user.email", "review@example.test"])?;
    git(
        repository,
        &["config", "user.name", "Review Queue Acceptance"],
    )?;
    fs::write(
        repository.join("review.txt"),
        format!("base content for repository {index}\n"),
    )?;
    git(repository, &["add", "review.txt"])?;
    git(repository, &["commit", "-m", "fixture base"])?;
    fs::write(
        repository.join("review.txt"),
        format!("initial review content for repository {index}\n"),
    )?;
    Ok(())
}

fn submit_workspace(
    store: &mut Store,
    workspace: &Path,
    topic: &str,
    title: &str,
) -> anyhow::Result<SubmissionResult> {
    let mut request = CaptureRequest {
        workspace_root: workspace.to_path_buf(),
        topic: topic.into(),
        brief: brief(title),
        origin_route_id: None,
        participating_repository_ids: Vec::new(),
        preflight_token: None,
    };
    let preflight = store.preflight_local_capture(&request)?;
    request.participating_repository_ids = preflight.participating_repository_ids;
    request.preflight_token = Some(preflight.preflight_token);
    store.ingest_local_capture(&request).map_err(Into::into)
}

fn create_peer_round(
    store: &mut Store,
    source: &review_queue_core::Round,
    topic: &str,
    title: &str,
) -> anyhow::Result<review_queue_core::Round> {
    match store.submit(Submission {
        collection: Collection::Local,
        topic_identity: format!("acceptance:{topic}"),
        brief: brief(title),
        manifest: source.manifest.clone(),
        origin_route: None,
        source_metadata: None,
        source_adapter: None,
    })? {
        SubmissionResult::Created(round) => Ok(round),
        result => bail!("expected peer round creation, received {result:?}"),
    }
}

fn command_error(error: crate::commands::CommandError) -> anyhow::Error {
    anyhow!(
        "{}: {} Next: {}",
        error.code,
        error.message,
        error.next_step
    )
}

fn brief(title: &str) -> ReviewBrief {
    ReviewBrief {
        title: title.into(),
        what: "Exercise explicit lifecycle operations through the packaged app binary.".into(),
        why: "Release acceptance must prove queue-only mutations leave source Git data intact."
            .into(),
        approach_alternatives:
            "Use restart-separated deterministic phases instead of mutating production app data."
                .into(),
        testing: "Request changes, complete, requeue, rank, resubmit, cancel, purge, and restart."
            .into(),
    }
}

fn active_rank_map(store: &Store) -> anyhow::Result<BTreeMap<String, i64>> {
    Ok(store
        .list(Some(Collection::Local), false)?
        .into_iter()
        .map(|round| (round.id, round.rank))
        .collect())
}

fn repository_states<const N: usize>(
    repositories: &[PathBuf; N],
) -> anyhow::Result<BTreeMap<String, RepositoryState>> {
    repositories
        .iter()
        .map(|repository| {
            let name = repository
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| anyhow!("fixture repository has no valid name"))?
                .to_owned();
            Ok((
                name,
                RepositoryState {
                    head: git_output(repository, &["rev-parse", "HEAD"])?,
                    tree: git_output(repository, &["rev-parse", "HEAD^{tree}"])?,
                    status: git_output(
                        repository,
                        &["status", "--porcelain=v1", "--untracked-files=all"],
                    )?,
                },
            ))
        })
        .collect()
}

fn lifecycle_event_names(store: &Store, id: &str) -> anyhow::Result<Vec<String>> {
    Ok(store
        .lifecycle_events(id)?
        .into_iter()
        .map(|event| match event.kind {
            LifecycleEventKind::RequestChanges => "request_changes",
            LifecycleEventKind::ApproveLocal => "approve_local",
            LifecycleEventKind::ApproveRemote => "approve_remote",
            LifecycleEventKind::Complete => "complete",
            LifecycleEventKind::Requeue => "requeue",
            LifecycleEventKind::Purge => "purge",
        })
        .map(str::to_owned)
        .collect())
}

fn git(repository: &Path, arguments: &[&str]) -> anyhow::Result<()> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(arguments)
        .output()
        .with_context(|| format!("run git {arguments:?}"))?;
    if !output.status.success() {
        bail!(
            "git {arguments:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

fn git_output(repository: &Path, arguments: &[&str]) -> anyhow::Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(arguments)
        .output()
        .with_context(|| format!("run git {arguments:?}"))?;
    if !output.status.success() {
        bail!(
            "git {arguments:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_phases_persist_across_reopen_and_never_touch_sources() {
        let temporary = tempfile::Builder::new()
            .prefix(ROOT_PREFIX)
            .tempdir()
            .unwrap();
        let first = phase_one(temporary.path()).unwrap();
        assert_eq!(
            first.request_complete_requeue_events,
            ["request_changes", "complete", "requeue"]
        );
        assert_eq!(
            first.delete_cancel_code.as_deref(),
            Some("confirmation_required")
        );
        assert_eq!(
            first.approve_cancel_code.as_deref(),
            Some("confirmation_required")
        );
        assert!(first.source_commits_and_files_unchanged);

        let second = phase_two(temporary.path()).unwrap();
        assert!(second.database_reopened);
        assert!(second.deleted_round_purged);
        assert!(second.approved_round_purged);
        assert!(second.superseded_round_retained);
        assert!(second.source_commits_and_files_unchanged);
        assert_eq!(second.active_ranks, first.active_ranks);
    }
}
