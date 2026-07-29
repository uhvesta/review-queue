//! Local Git workspace capture.
//!
//! Capture uses a temporary Git index while preparing commits, so a failed
//! capture cannot disturb the caller's staging area. After every repository
//! succeeds, its real index is synchronized to the newly committed tree. That
//! is the same coherent post-commit state Git itself leaves behind and avoids
//! treating captured untracked files as deletions on a repeat submission.

use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

use crate::{DomainError, RepositorySnapshot, ReviewBrief, WorkspaceManifest};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CaptureRequest {
    pub workspace_root: PathBuf,
    pub topic: String,
    pub brief: ReviewBrief,
    /// Optional registered AgentRoute selected for this capture. The Store
    /// resolves this ID to the complete token-free route and provenance before
    /// any Git or SQLite mutation. It is included in the preflight fingerprint
    /// so a route cannot be silently changed between preview and capture.
    #[serde(default)]
    pub origin_route_id: Option<String>,
    /// Empty during initial discovery; otherwise the exact repositories which
    /// the user chose to participate.
    #[serde(default)]
    pub participating_repository_ids: Vec<String>,
    /// Binds the later mutation to the exact form, selection, and Git state
    /// returned by preflight.
    #[serde(default)]
    pub preflight_token: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Preflight {
    pub repositories: Vec<PreflightRepository>,
    pub before_fingerprint: String,
    pub participating_repository_ids: Vec<String>,
    #[serde(default)]
    pub origin_route_id: Option<String>,
    pub preflight_token: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PreflightRepository {
    pub root: PathBuf,
    pub repository_id: String,
    pub branch: String,
    pub head_sha: String,
    pub status: String,
    pub has_changes: bool,
    pub participating: bool,
}

#[derive(Debug)]
struct CapturedRepository {
    preflight: PreflightRepository,
    new_head: String,
    index_path: PathBuf,
    original_index: Option<Vec<u8>>,
}

/// A prepared multi-repository capture whose refs can still be restored.
///
/// Callers which need to persist the manifest elsewhere first keep this guard
/// alive until that persistence succeeds, then call [`PendingCapture::commit`].
/// Calling [`PendingCapture::abort`] restores every advanced ref with
/// compare-and-swap semantics while the caller's original index/worktree are
/// still intact.
#[derive(Debug)]
pub struct PendingCapture {
    manifest: WorkspaceManifest,
    committed: Vec<CapturedRepository>,
    armed: bool,
}

impl PendingCapture {
    pub fn manifest(&self) -> &WorkspaceManifest {
        &self.manifest
    }

    pub fn finalize_indexes(&mut self) -> Result<(), DomainError> {
        for captured in &self.committed {
            if let Err(problem) = sync_index_to_head(&captured.preflight) {
                let unsafe_repositories = restore_captured(&self.committed);
                self.armed = false;
                return Err(compensation_error(problem, unsafe_repositories));
            }
        }
        Ok(())
    }

    pub fn seal(mut self) -> WorkspaceManifest {
        self.armed = false;
        self.manifest.clone()
    }

    pub fn commit(mut self) -> Result<WorkspaceManifest, DomainError> {
        self.finalize_indexes()?;
        Ok(self.seal())
    }

    pub fn abort(mut self, problem: DomainError) -> DomainError {
        let error = compensation_error(problem, restore_captured(&self.committed));
        self.armed = false;
        error
    }
}

impl Drop for PendingCapture {
    fn drop(&mut self) {
        if self.armed {
            let _ = restore_captured(&self.committed);
            self.armed = false;
        }
    }
}

/// Finds every non-bare Git worktree rooted under `workspace_root`.
pub fn discover_repositories(
    workspace_root: impl AsRef<Path>,
) -> Result<Vec<PathBuf>, DomainError> {
    let workspace_root = canonical_workspace(workspace_root.as_ref())?;
    let mut candidates = BTreeSet::new();
    discover_under(&workspace_root, &mut candidates)?;
    // A workspace which is itself a repository has a `.git` entry and is
    // found above.  Canonical paths make nested repository de-duplication
    // deterministic.
    if candidates.is_empty() {
        return Err(error(
            "No Git repositories were found in this workspace.",
            "No source files or Git refs were changed.",
            "Choose a workspace containing at least one Git repository and retry.",
            "repositories_required",
        ));
    }
    Ok(candidates.into_iter().map(PathBuf::from).collect())
}

/// Checks all conditions which can be checked before any ref is changed.
pub fn preflight(request: &CaptureRequest) -> Result<Preflight, DomainError> {
    request.brief.validate()?;
    validate_topic(&request.topic)?;
    let workspace = canonical_workspace(&request.workspace_root)?;
    let repositories = discover_repositories(&workspace)?;
    let mut result = Vec::with_capacity(repositories.len());
    for root in repositories {
        let head_sha = git(&root, ["rev-parse", "HEAD"])?;
        let branch = git(&root, ["symbolic-ref", "--quiet", "--short", "HEAD"]).map_err(|_| {
            error(
                &format!("Repository '{}' is in detached HEAD state.", root.display()),
                "No source files or Git refs were changed.",
                "Check out the branch that should receive the review commit, then retry.",
                "detached_head",
            )
        })?;
        let status = git(&root, ["status", "--porcelain=v1", "--untracked-files=all"])?;
        // An unchanged repository does not create a commit, so its author
        // configuration is irrelevant to this submission.
        if !status.is_empty() {
            git(&root, ["var", "GIT_AUTHOR_IDENT"]).map_err(|_| {
                error(
                    &format!(
                        "Repository '{}' has no usable Git author identity.",
                        root.display()
                    ),
                    "No source files or Git refs were changed.",
                    "Configure user.name and user.email for this repository, then retry.",
                    "git_identity_required",
                )
            })?;
        }
        result.push(PreflightRepository {
            repository_id: repository_id(&workspace, &root)?,
            root,
            branch,
            head_sha,
            has_changes: !status.is_empty(),
            status,
            participating: false,
        });
    }
    result.sort_by(|a, b| a.repository_id.cmp(&b.repository_id));
    let requested = if request.participating_repository_ids.is_empty() {
        result
            .iter()
            .map(|repository| repository.repository_id.clone())
            .collect::<Vec<_>>()
    } else {
        let mut unique = BTreeSet::new();
        for repository_id in &request.participating_repository_ids {
            if repository_id.trim().is_empty() || !unique.insert(repository_id.clone()) {
                return Err(error(
                    "Repository participation contains an empty or duplicate ID.",
                    "No source files or Git refs were changed.",
                    "Select each participating repository exactly once and run preflight again.",
                    "repository_selection_invalid",
                ));
            }
            if !result
                .iter()
                .any(|repository| repository.repository_id == *repository_id)
            {
                return Err(error(
                    &format!(
                        "Selected repository '{}' is no longer available.",
                        repository_id
                    ),
                    "No source files or Git refs were changed.",
                    "Refresh repository preflight and choose from the detected repositories.",
                    "repository_selection_stale",
                ));
            }
        }
        let mut selected = request.participating_repository_ids.clone();
        selected.sort();
        selected
    };
    if requested.is_empty() {
        return Err(error(
            "At least one repository must participate in capture.",
            "No source files or Git refs were changed.",
            "Select one or more detected repositories and run preflight again.",
            "repository_selection_required",
        ));
    }
    for repository in &mut result {
        repository.participating = requested.contains(&repository.repository_id);
    }
    let before_fingerprint = fingerprint(
        result
            .iter()
            .filter(|repository| repository.participating)
            .map(|r| format!("{}\0{}\0{}", r.repository_id, r.head_sha, r.status)),
    );
    let preflight_token = request_fingerprint(&workspace, request, &requested, &before_fingerprint);
    Ok(Preflight {
        repositories: result,
        before_fingerprint,
        participating_repository_ids: requested,
        origin_route_id: request.origin_route_id.clone(),
        preflight_token,
    })
}

/// Commits all captured changes and returns an immutable manifest.
///
/// If a repository fails after earlier commits succeeded, those commits are
/// reverted with compare-and-swap `update-ref` operations.  We never reset a
/// worktree or index, and a ref that advanced independently is never moved.
pub fn capture(request: &CaptureRequest) -> Result<WorkspaceManifest, DomainError> {
    prepare_capture(request)?.commit()
}

/// Creates topic-tagged commits but deliberately leaves the caller's real
/// indexes untouched until the returned guard is committed.
pub fn prepare_capture(request: &CaptureRequest) -> Result<PendingCapture, DomainError> {
    let workspace = canonical_workspace(&request.workspace_root)?;
    let plan = preflight(request)?;
    if let Some(expected) = request.preflight_token.as_deref()
        && expected != plan.preflight_token
    {
        return Err(error(
            "The repository preflight is stale for this submission.",
            "No source files or Git refs were changed.",
            "Run repository preflight again after the latest form, selection, or workspace change.",
            "preflight_stale",
        ));
    }
    let mut committed: Vec<CapturedRepository> = Vec::new();
    let mut snapshots = Vec::with_capacity(plan.participating_repository_ids.len());

    for repo in plan
        .repositories
        .iter()
        .filter(|repository| repository.participating)
    {
        if let Err(problem) = unchanged_since_preflight(repo) {
            return Err(compensate_owned(problem, &committed));
        }
        let head = if repo.has_changes {
            let (index_path, original_index) = index_backup(repo)?;
            match commit_snapshot(repo, request) {
                Ok(head) => {
                    committed.push(CapturedRepository {
                        preflight: repo.clone(),
                        new_head: head.clone(),
                        index_path,
                        original_index,
                    });
                    head
                }
                Err(problem) => return Err(compensate_owned(problem, &committed)),
            }
        } else {
            repo.head_sha.clone()
        };
        match snapshot(&workspace, repo, head) {
            Ok(snapshot) => snapshots.push(snapshot),
            Err(problem) => return Err(compensate_owned(problem, &committed)),
        }
    }
    let after_fingerprint = fingerprint(
        snapshots
            .iter()
            .map(|r| format!("{}\0{}\0{}", r.repository_id, r.head_sha, r.object_checksum)),
    );
    Ok(PendingCapture {
        manifest: WorkspaceManifest {
            workspace_id: digest(workspace.to_string_lossy().as_bytes()),
            workspace_root: workspace.to_string_lossy().into_owned(),
            topic: request.topic.trim().into(),
            repositories: snapshots,
            before_fingerprint: plan.before_fingerprint,
            after_fingerprint,
            created_at: Utc::now(),
        },
        committed,
        armed: true,
    })
}

fn request_fingerprint(
    workspace: &Path,
    request: &CaptureRequest,
    participating_repository_ids: &[String],
    before_fingerprint: &str,
) -> String {
    fingerprint(
        [
            workspace.to_string_lossy().into_owned(),
            request.topic.trim().to_owned(),
            request.brief.title.trim().to_owned(),
            request.brief.what.trim().to_owned(),
            request.brief.why.trim().to_owned(),
            request.brief.approach_alternatives.trim().to_owned(),
            request.brief.testing.trim().to_owned(),
            request
                .origin_route_id
                .as_deref()
                .unwrap_or_default()
                .trim()
                .to_owned(),
            before_fingerprint.to_owned(),
        ]
        .into_iter()
        .chain(participating_repository_ids.iter().cloned()),
    )
}

fn discover_under(path: &Path, found: &mut BTreeSet<String>) -> Result<(), DomainError> {
    let entries = fs::read_dir(path).map_err(|e| filesystem_error(path, e))?;
    let is_repo = path.join(".git").exists();
    if is_repo {
        let root = PathBuf::from(git(path, ["rev-parse", "--show-toplevel"])?);
        found.insert(canonical_workspace(&root)?.to_string_lossy().into_owned());
        // Still descend: nested repositories are independently participating.
    }
    for entry in entries {
        let entry = entry.map_err(|e| filesystem_error(path, e))?;
        let file_type = entry
            .file_type()
            .map_err(|e| filesystem_error(&entry.path(), e))?;
        if file_type.is_dir() && entry.file_name() != ".git" {
            discover_under(&entry.path(), found)?;
        }
    }
    Ok(())
}

fn unchanged_since_preflight(repo: &PreflightRepository) -> Result<(), DomainError> {
    let head = git(&repo.root, ["rev-parse", "HEAD"])?;
    let status = git(
        &repo.root,
        ["status", "--porcelain=v1", "--untracked-files=all"],
    )?;
    if head != repo.head_sha || status != repo.status {
        return Err(error(
            &format!(
                "Repository '{}' changed while capture was being prepared.",
                repo.repository_id
            ),
            "No additional source files were changed by Review Queue.",
            "Retry capture after the workspace is stable.",
            "workspace_changed_during_capture",
        ));
    }
    Ok(())
}

fn commit_snapshot(
    repo: &PreflightRepository,
    request: &CaptureRequest,
) -> Result<String, DomainError> {
    let temp = TempDir::new().map_err(|e| filesystem_error(&repo.root, e))?;
    let index = temp.path().join("capture.index");
    git_with_index(&repo.root, &index, ["read-tree", "HEAD"])?;
    git_with_index(&repo.root, &index, ["add", "-A", "--", "."])?;
    let subject = format!("{}: {}", request.topic.trim(), request.brief.title.trim());
    let body = format!(
        "What:\n{}\n\nWhy:\n{}\n\nApproach / alternatives:\n{}\n\nTesting:\n{}",
        request.brief.what.trim(),
        request.brief.why.trim(),
        request.brief.approach_alternatives.trim(),
        request.brief.testing.trim(),
    );
    git_with_index(&repo.root, &index, ["commit", "-m", &subject, "-m", &body]).map_err(|e| {
        error(
            &format!(
                "Review commit failed in repository '{}': {}",
                repo.repository_id, e.error.what_happened
            ),
            "Source files and your original staging area are unchanged.",
            "Fix the reported Git problem and retry capture.",
            "submission_commit_failed",
        )
    })?;
    let head = git(&repo.root, ["rev-parse", "HEAD"])?;
    if head == repo.head_sha {
        return Err(error(
            "Git reported a successful commit but HEAD did not advance.",
            "Source files are unchanged.",
            "Inspect the repository's Git hooks and retry capture.",
            "submission_commit_failed",
        ));
    }
    Ok(head)
}

fn compensate_owned(problem: DomainError, committed: &[CapturedRepository]) -> DomainError {
    compensation_error(problem, restore_captured(committed))
}

fn compensation_error(problem: DomainError, unsafe_repositories: Vec<String>) -> DomainError {
    if unsafe_repositories.is_empty() {
        error(
            &problem.error.what_happened,
            "Source files, original staging indexes, and Git refs were restored; no review round was created.",
            &problem.error.next_step,
            &problem.error.code,
        )
    } else {
        error(
            &format!(
                "{} Compensation could not safely restore commit(s) in {} because those refs or indexes changed again.",
                problem.error.what_happened,
                unsafe_repositories.join(", ")
            ),
            "Source files are safe; the named review commits or indexes require inspection.",
            "Inspect the named repositories, then retry capture after resolving their Git state.",
            "capture_compensation_required",
        )
    }
}

fn restore_captured(committed: &[CapturedRepository]) -> Vec<String> {
    let mut unsafe_repositories = Vec::new();
    for captured in committed.iter().rev() {
        let repo = &captured.preflight;
        let ref_restored = git(
            &repo.root,
            [
                "update-ref",
                &format!("refs/heads/{}", repo.branch),
                &repo.head_sha,
                &captured.new_head,
            ],
        )
        .is_ok();
        let index_restored = match &captured.original_index {
            Some(bytes) => fs::write(&captured.index_path, bytes).is_ok(),
            None if captured.index_path.exists() => fs::remove_file(&captured.index_path).is_ok(),
            None => true,
        };
        if !ref_restored || !index_restored {
            unsafe_repositories.push(repo.repository_id.clone());
        }
    }
    unsafe_repositories
}

fn index_backup(repo: &PreflightRepository) -> Result<(PathBuf, Option<Vec<u8>>), DomainError> {
    let raw = git(&repo.root, ["rev-parse", "--git-path", "index"])?;
    let path = {
        let candidate = PathBuf::from(raw);
        if candidate.is_absolute() {
            candidate
        } else {
            repo.root.join(candidate)
        }
    };
    let bytes = match fs::read(&path) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(filesystem_error(&path, error)),
    };
    Ok((path, bytes))
}

fn snapshot(
    workspace: &Path,
    repo: &PreflightRepository,
    head_sha: String,
) -> Result<RepositorySnapshot, DomainError> {
    let object_checksum = git(&repo.root, ["rev-parse", &format!("{head_sha}^{{tree}}")])?;
    let remote_fingerprint = git(&repo.root, ["remote", "get-url", "origin"])
        .ok()
        .filter(|s| !s.is_empty())
        .map(|s| digest(s.as_bytes()));
    Ok(RepositorySnapshot {
        repository_id: repo.repository_id.clone(),
        root: repository_id(workspace, &repo.root)?,
        branch: repo.branch.clone(),
        base_sha: repo.head_sha.clone(),
        head_sha,
        remote_fingerprint,
        object_checksum,
    })
}

fn sync_index_to_head(repo: &PreflightRepository) -> Result<(), DomainError> {
    git(&repo.root, ["read-tree", "HEAD"]).map(|_| ()).map_err(|e| error(
        &format!("Review commits were created but Git could not finalize the index for repository '{}': {}", repo.repository_id, e.error.what_happened),
        "Source files are unchanged. Review Queue will restore created refs when it can do so safely.",
        "Close other Git operations in that repository and retry capture.",
        "submission_index_finalize_failed",
    ))
}

fn git<const N: usize>(root: &Path, args: [&str; N]) -> Result<String, DomainError> {
    run_git(root, None, args)
}
fn git_with_index<const N: usize>(
    root: &Path,
    index: &Path,
    args: [&str; N],
) -> Result<String, DomainError> {
    run_git(root, Some(index), args)
}
fn run_git<const N: usize>(
    root: &Path,
    index: Option<&Path>,
    args: [&str; N],
) -> Result<String, DomainError> {
    let mut command = Command::new("git");
    command.arg("-C").arg(root).args(args);
    if let Some(index) = index {
        command.env("GIT_INDEX_FILE", index);
    }
    command.env_remove("GIT_DIR").env_remove("GIT_WORK_TREE");
    let output = command.output().map_err(|e| {
        error(
            &format!("Could not run Git in '{}': {e}", root.display()),
            "No source files or Git refs were changed.",
            "Install Git or repair the repository, then retry.",
            "git_unavailable",
        )
    })?;
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout)
            .trim_end()
            .to_owned());
    }
    let detail = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    Err(error(
        &format!(
            "Git failed in '{}': {}",
            root.display(),
            if detail.is_empty() {
                "unknown Git error"
            } else {
                &detail
            }
        ),
        "Source files are unchanged.",
        "Fix the Git error and retry capture.",
        "git_command_failed",
    ))
}

fn canonical_workspace(path: &Path) -> Result<PathBuf, DomainError> {
    fs::canonicalize(path).map_err(|e| {
        error(
            &format!("Workspace '{}' is unavailable: {e}", path.display()),
            "No source files or Git refs were changed.",
            "Choose an existing workspace and retry.",
            "workspace_unavailable",
        )
    })
}
fn repository_id(workspace: &Path, root: &Path) -> Result<String, DomainError> {
    root.strip_prefix(workspace)
        .map_err(|_| {
            error(
                "A discovered repository is outside the requested workspace.",
                "No source files or Git refs were changed.",
                "Choose a single workspace containing all participating repositories.",
                "repository_outside_workspace",
            )
        })
        .map(|p| {
            let s = p.to_string_lossy();
            if s.is_empty() {
                ".".to_owned()
            } else {
                s.into_owned()
            }
        })
}
fn validate_topic(topic: &str) -> Result<(), DomainError> {
    if topic.trim().is_empty() || topic.contains('\n') || topic.contains('\r') {
        Err(error(
            "A single-line review topic is required.",
            "No source files or Git refs were changed.",
            "Provide a non-empty topic and retry capture.",
            "topic_required",
        ))
    } else {
        Ok(())
    }
}
fn fingerprint(values: impl Iterator<Item = String>) -> String {
    digest(values.collect::<Vec<_>>().join("\n").as_bytes())
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn error(what: &str, safety: &str, next: &str, code: &str) -> DomainError {
    DomainError::actionable(what, safety, next, code)
}
fn filesystem_error(path: &Path, err: impl std::fmt::Display) -> DomainError {
    error(
        &format!("Could not read workspace path '{}': {err}", path.display()),
        "No source files or Git refs were changed.",
        "Fix access to the workspace and retry capture.",
        "workspace_unavailable",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn run(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }
    fn output(dir: &Path, args: &[&str]) -> String {
        String::from_utf8(
            Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(args)
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .into()
    }
    fn repo(parent: &Path, name: &str) -> PathBuf {
        let path = parent.join(name);
        fs::create_dir_all(&path).unwrap();
        run(&path, &["init"]);
        run(&path, &["config", "user.email", "test@example.com"]);
        run(&path, &["config", "user.name", "Test User"]);
        fs::write(path.join("initial.txt"), "initial\n").unwrap();
        run(&path, &["add", "."]);
        run(&path, &["commit", "-m", "initial"]);
        path
    }
    fn request(root: &Path) -> CaptureRequest {
        CaptureRequest {
            workspace_root: root.into(),
            topic: "parser-v2".into(),
            brief: ReviewBrief {
                title: "Capture parser changes".into(),
                what: "Improve parser".into(),
                why: "Needed now".into(),
                approach_alternatives: String::new(),
                testing: String::new(),
            },
            origin_route_id: None,
            participating_repository_ids: Vec::new(),
            preflight_token: None,
        }
    }

    #[test]
    fn discovers_nested_repositories_and_commits_all_change_kinds_without_touching_index() {
        let workspace = tempfile::tempdir().unwrap();
        let app = repo(workspace.path(), "app");
        let parser = repo(&workspace.path().join("packages"), "parser");
        fs::write(app.join("initial.txt"), "edited\n").unwrap();
        run(&app, &["add", "initial.txt"]);
        fs::write(app.join("untracked.txt"), "new\n").unwrap();
        fs::write(parser.join("initial.txt"), "edited\n").unwrap();
        let manifest = capture(&request(workspace.path())).unwrap();
        assert_eq!(manifest.repositories.len(), 2);
        assert!(
            manifest
                .repositories
                .iter()
                .all(|r| r.head_sha != r.base_sha && !r.object_checksum.is_empty())
        );
        assert_eq!(
            output(&app, &["show", "--format=%s", "-s", "HEAD"]),
            "parser-v2: Capture parser changes"
        );
        // Capture does not rewrite files and leaves the repository in Git's
        // normal post-commit state, so repeat submission sees no phantom
        // deletion from a stale index.
        assert!(output(&app, &["status", "--porcelain"]).is_empty());
        assert_eq!(output(&app, &["show", "HEAD:untracked.txt"]), "new");
    }

    #[test]
    fn unchanged_repository_pins_existing_head_without_empty_commit() {
        let workspace = tempfile::tempdir().unwrap();
        let app = repo(workspace.path(), "app");
        let before = output(&app, &["rev-parse", "HEAD"]);
        let manifest = capture(&request(workspace.path())).unwrap();
        assert_eq!(manifest.repositories[0].head_sha, before);
        assert_eq!(manifest.repositories[0].base_sha, before);
        assert_eq!(output(&app, &["rev-list", "--count", "HEAD"]), "1");
    }

    #[test]
    fn invalid_later_repository_compensates_earlier_commit() {
        let workspace = tempfile::tempdir().unwrap();
        let app = repo(workspace.path(), "app");
        let bad = repo(workspace.path(), "zbad");
        fs::write(app.join("new.txt"), "new\n").unwrap();
        fs::write(bad.join("new.txt"), "new\n").unwrap();
        // A failing pre-commit hook only affects the second repository.
        let hooks = bad.join(".git/hooks");
        fs::write(hooks.join("pre-commit"), "#!/bin/sh\nexit 1\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(hooks.join("pre-commit"), fs::Permissions::from_mode(0o755))
                .unwrap();
        }
        let app_before = output(&app, &["rev-parse", "HEAD"]);
        let err = capture(&request(workspace.path())).unwrap_err();
        assert_eq!(err.error.code, "submission_commit_failed");
        assert_eq!(output(&app, &["rev-parse", "HEAD"]), app_before);
        assert_eq!(output(&app, &["status", "--porcelain"]), "?? new.txt");
    }

    #[test]
    fn pending_capture_abort_restores_ref_and_exact_index_state() {
        let workspace = tempfile::tempdir().unwrap();
        let app = repo(workspace.path(), "app");
        fs::write(app.join("initial.txt"), "staged\n").unwrap();
        run(&app, &["add", "initial.txt"]);
        fs::write(app.join("initial.txt"), "staged plus unstaged\n").unwrap();
        fs::write(app.join("untracked.txt"), "new\n").unwrap();
        let head_before = output(&app, &["rev-parse", "HEAD"]);
        let status_before = output(&app, &["status", "--porcelain=v1", "--untracked-files=all"]);

        let pending = prepare_capture(&request(workspace.path())).unwrap();
        assert_ne!(output(&app, &["rev-parse", "HEAD"]), head_before);
        let error = pending.abort(error("Persistence failed.", "safe", "retry", "test"));

        assert_eq!(error.error.code, "test");
        assert_eq!(output(&app, &["rev-parse", "HEAD"]), head_before);
        assert_eq!(
            output(&app, &["status", "--porcelain=v1", "--untracked-files=all"]),
            status_before
        );
    }

    #[test]
    fn capture_commit_body_contains_the_canonical_brief() {
        let workspace = tempfile::tempdir().unwrap();
        let app = repo(workspace.path(), "app");
        fs::write(app.join("new.txt"), "new\n").unwrap();
        let mut input = request(workspace.path());
        input.brief.approach_alternatives = "Use a two-phase capture.".into();
        input.brief.testing = "Ran rollback tests.".into();
        capture(&input).unwrap();
        let body = output(&app, &["show", "-s", "--format=%B", "HEAD"]);
        assert!(body.contains("What:\nImprove parser"));
        assert!(body.contains("Why:\nNeeded now"));
        assert!(body.contains("Approach / alternatives:\nUse a two-phase capture."));
        assert!(body.contains("Testing:\nRan rollback tests."));
    }

    #[test]
    fn preflight_token_binds_form_selection_and_git_state() {
        let workspace = tempfile::tempdir().unwrap();
        let app = repo(workspace.path(), "app");
        let tools = repo(workspace.path(), "tools");
        fs::write(app.join("app.txt"), "new\n").unwrap();
        fs::write(tools.join("tools.txt"), "new\n").unwrap();
        let mut input = request(workspace.path());
        let all = preflight(&input).unwrap();
        assert_eq!(all.participating_repository_ids, vec!["app", "tools"]);

        input.participating_repository_ids = vec!["app".into()];
        let app_only = preflight(&input).unwrap();
        assert_ne!(all.preflight_token, app_only.preflight_token);
        let mut routed = input.clone();
        routed.origin_route_id = Some("route-a".into());
        let route_a = preflight(&routed).unwrap();
        routed.origin_route_id = Some("route-b".into());
        let route_b = preflight(&routed).unwrap();
        assert_ne!(app_only.preflight_token, route_a.preflight_token);
        assert_ne!(route_a.preflight_token, route_b.preflight_token);
        assert!(
            app_only
                .repositories
                .iter()
                .find(|repository| repository.repository_id == "app")
                .unwrap()
                .participating
        );
        assert!(
            !app_only
                .repositories
                .iter()
                .find(|repository| repository.repository_id == "tools")
                .unwrap()
                .participating
        );

        input.preflight_token = Some(app_only.preflight_token);
        input.brief.why = "The form changed.".into();
        let error = prepare_capture(&input).unwrap_err();
        assert_eq!(error.error.code, "preflight_stale");
        assert_eq!(output(&app, &["rev-list", "--count", "HEAD"]), "1");
        assert_eq!(output(&tools, &["rev-list", "--count", "HEAD"]), "1");

        routed.preflight_token = Some(route_b.preflight_token);
        routed.origin_route_id = Some("route-a".into());
        let error = prepare_capture(&routed).unwrap_err();
        assert_eq!(error.error.code, "preflight_stale");
        assert_eq!(output(&app, &["rev-list", "--count", "HEAD"]), "1");
        assert_eq!(output(&tools, &["rev-list", "--count", "HEAD"]), "1");
    }
}
