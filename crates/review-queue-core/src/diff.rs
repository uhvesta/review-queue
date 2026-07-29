//! Immutable, read-only materialization of the Git objects pinned in a review
//! manifest.  This module never consults the worktree or index for content.

use std::{path::Path, process::Command};

use base64::Engine as _;
use serde::{Deserialize, Serialize};

use crate::{DomainError, RepositorySnapshot, Round, WorkspaceManifest};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct MaterializedDiff {
    pub repositories: Vec<RepositoryDiff>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct RepositoryDiff {
    pub repository_id: String,
    pub root: String,
    pub base_sha: String,
    pub head_sha: String,
    /// Files are in Git's deterministic tree-diff order.
    pub files: Vec<DiffFile>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DiffFileStatus {
    Added,
    Deleted,
    Modified,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct DiffFile {
    /// Present here as well as on `RepositoryDiff` so a selected file/hunk is
    /// always unambiguous when two repositories contain `src/main.rs`.
    pub repository_id: String,
    pub old_path: Option<String>,
    pub new_path: Option<String>,
    #[serde(default)]
    pub old_blob_sha: Option<String>,
    #[serde(default)]
    pub new_blob_sha: Option<String>,
    pub status: DiffFileStatus,
    pub is_binary: bool,
    /// The complete Git patch section, including binary payloads when present.
    pub patch: String,
    pub hunks: Vec<DiffHunk>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct DiffHunk {
    pub repository_id: String,
    pub old_start: u32,
    pub old_lines: u32,
    pub new_start: u32,
    pub new_lines: u32,
    pub header: String,
    pub lines: Vec<DiffLine>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct DiffLine {
    #[serde(rename = "type")]
    pub kind: DiffLineKind,
    pub content: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct PinnedFileContent {
    pub repository_id: String,
    pub path: String,
    pub side: String,
    pub blob_sha: String,
    pub is_binary: bool,
    pub content: Option<String>,
    /// Complete blob bytes for binary-safe machine federation.
    pub content_base64: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DiffLineKind {
    Context,
    Addition,
    Deletion,
}

pub fn materialize_round(round: &Round) -> Result<MaterializedDiff, DomainError> {
    materialize_manifest(&round.manifest)
}

/// Loads a complete file blob from the round's pinned base or head commit.
/// The live worktree and index are never consulted.
pub fn materialize_file(
    round: &Round,
    repository_id: &str,
    path: &str,
    side: &str,
) -> Result<PinnedFileContent, DomainError> {
    if path.trim().is_empty()
        || path.starts_with('/')
        || path.split('/').any(|component| component == "..")
    {
        return Err(DomainError::actionable(
            "The requested review file path is invalid.",
            "No source file or review state was changed.",
            "Choose a file from the captured review tree.",
            "invalid_review_file_path",
        ));
    }
    let snapshot = round
        .manifest
        .repositories
        .iter()
        .find(|snapshot| snapshot.repository_id == repository_id)
        .ok_or_else(|| {
            DomainError::actionable(
                "The requested repository is not part of this review round.",
                "No source file or review state was changed.",
                "Choose a repository from the captured review tree.",
                "review_repository_not_found",
            )
        })?;
    let revision = match side.to_ascii_lowercase().as_str() {
        "left" | "base" => &snapshot.base_sha,
        "right" | "head" => &snapshot.head_sha,
        _ => {
            return Err(DomainError::actionable(
                "The requested review side is invalid.",
                "No source file or review state was changed.",
                "Choose the base (LEFT) or head (RIGHT) side.",
                "invalid_review_side",
            ));
        }
    };
    let captured_root = Path::new(&snapshot.root);
    let root = if captured_root.is_absolute() {
        captured_root.to_path_buf()
    } else {
        Path::new(&round.manifest.workspace_root).join(captured_root)
    };
    ensure_revision(&root, snapshot, revision, side)?;
    let blob_sha = blob_sha(&root, revision, path).ok_or_else(|| {
        DomainError::actionable(
            format!("'{path}' does not exist on the requested pinned side."),
            "No source file or review state was changed.",
            "Choose a file and side present in this review round.",
            "pinned_file_not_found",
        )
    })?;
    let output = Command::new("git")
        .arg("-C")
        .arg(&root)
        .args(["cat-file", "blob", &blob_sha])
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()
        .map_err(|_| unavailable(snapshot))?;
    if !output.status.success() {
        return Err(DomainError::actionable(
            "The pinned file blob could not be read.",
            "No source file or review state was changed.",
            "Restore the captured Git objects and retry.",
            "pinned_blob_unavailable",
        ));
    }
    let content_base64 = base64::engine::general_purpose::STANDARD.encode(&output.stdout);
    let content = String::from_utf8(output.stdout).ok();
    Ok(PinnedFileContent {
        repository_id: repository_id.into(),
        path: path.into(),
        side: side.to_ascii_uppercase(),
        blob_sha,
        is_binary: content.is_none(),
        content,
        content_base64,
    })
}

/// Loads precisely the `base_sha..head_sha` object diff for every repository
/// in manifest order. Neither the live checkout nor its index participates.
pub fn materialize_manifest(manifest: &WorkspaceManifest) -> Result<MaterializedDiff, DomainError> {
    let repositories = manifest
        .repositories
        .iter()
        .map(|snapshot| materialize_repository(Path::new(&manifest.workspace_root), snapshot))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(MaterializedDiff { repositories })
}

fn materialize_repository(
    workspace_root: &Path,
    snapshot: &RepositorySnapshot,
) -> Result<RepositoryDiff, DomainError> {
    let captured_root = Path::new(&snapshot.root);
    let root = if captured_root.is_absolute() {
        captured_root.to_path_buf()
    } else {
        workspace_root.join(captured_root)
    };
    ensure_revision(&root, snapshot, &snapshot.base_sha, "base")?;
    ensure_revision(&root, snapshot, &snapshot.head_sha, "head")?;
    let patch = git(
        &root,
        snapshot,
        [
            "diff",
            "--no-ext-diff",
            "--no-renames",
            "--binary",
            "--full-index",
            "--unified=3",
            "--no-color",
            "--format=",
            &snapshot.base_sha,
            &snapshot.head_sha,
            "--",
        ],
    )?;
    let mut files = parse_patch(&snapshot.repository_id, &patch);
    for file in &mut files {
        file.old_blob_sha = file
            .old_path
            .as_deref()
            .and_then(|path| blob_sha(&root, &snapshot.base_sha, path));
        file.new_blob_sha = file
            .new_path
            .as_deref()
            .and_then(|path| blob_sha(&root, &snapshot.head_sha, path));
    }
    Ok(RepositoryDiff {
        repository_id: snapshot.repository_id.clone(),
        root: snapshot.root.clone(),
        base_sha: snapshot.base_sha.clone(),
        head_sha: snapshot.head_sha.clone(),
        files,
    })
}

fn blob_sha(root: &Path, revision: &str, path: &str) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--verify", &format!("{revision}:{path}")])
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout)
        .ok()
        .map(|value| value.trim().to_owned())
}

fn ensure_revision(
    root: &Path,
    snapshot: &RepositorySnapshot,
    sha: &str,
    label: &str,
) -> Result<(), DomainError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .arg("cat-file")
        .arg("-e")
        .arg(format!("{sha}^{{commit}}"))
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()
        .map_err(|_| unavailable(snapshot))?;
    if !output.status.success() {
        return Err(DomainError::actionable(
            format!(
                "The pinned {label} revision for repository '{}' is no longer available locally.",
                snapshot.repository_id
            ),
            "No source files, Git refs, or review records were changed.",
            "Fetch or restore the captured Git objects, then open the review again.",
            "pinned_revision_unavailable",
        ));
    }
    Ok(())
}

fn git(
    root: &Path,
    snapshot: &RepositorySnapshot,
    args: impl IntoIterator<Item = impl AsRef<std::ffi::OsStr>>,
) -> Result<String, DomainError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()
        .map_err(|_| unavailable(snapshot))?;
    if !output.status.success() {
        return Err(DomainError::actionable(
            format!(
                "Could not materialize the immutable diff for repository '{}'.",
                snapshot.repository_id
            ),
            "No source files, Git refs, or review records were changed.",
            "Verify that the repository is accessible and that the captured revisions still exist locally.",
            "diff_materialization_failed",
        ));
    }
    String::from_utf8(output.stdout).map_err(|_| {
        DomainError::actionable(
            format!(
                "The immutable diff for repository '{}' is not valid UTF-8.",
                snapshot.repository_id
            ),
            "No source files, Git refs, or review records were changed.",
            "Review this binary-only change through its Git metadata or use a UTF-8 filename.",
            "diff_not_utf8",
        )
    })
}

fn unavailable(snapshot: &RepositorySnapshot) -> DomainError {
    DomainError::actionable(
        format!(
            "Repository '{}' is unavailable at '{}'.",
            snapshot.repository_id, snapshot.root
        ),
        "No source files, Git refs, or review records were changed.",
        "Reconnect or restore the repository at its captured path, then retry.",
        "repository_unavailable",
    )
}

fn parse_patch(repository_id: &str, patch: &str) -> Vec<DiffFile> {
    let mut files = Vec::new();
    let mut section = String::new();
    for line in patch.split_inclusive('\n') {
        if line.starts_with("diff --git ") && !section.is_empty() {
            files.push(parse_file(repository_id, &section));
            section.clear();
        }
        section.push_str(line);
    }
    if !section.is_empty() {
        files.push(parse_file(repository_id, &section));
    }
    files
}

fn parse_file(repository_id: &str, patch: &str) -> DiffFile {
    let mut old_path = None;
    let mut new_path = None;
    for line in patch.lines() {
        if let Some(path) = line.strip_prefix("--- ") {
            old_path = diff_path(path);
        }
        if let Some(path) = line.strip_prefix("+++ ") {
            new_path = diff_path(path);
        }
    }
    let status = if patch.contains("new file mode ") {
        DiffFileStatus::Added
    } else if patch.contains("deleted file mode ") {
        DiffFileStatus::Deleted
    } else {
        DiffFileStatus::Modified
    };
    // Binary sections do not have `---` / `+++` headers.  Git's diff header
    // is still sufficient for ordinary (unquoted) paths, which is the format
    // emitted by a normal captured workspace path.
    if old_path.is_none()
        && new_path.is_none()
        && let Some((old, new)) = patch
            .lines()
            .next()
            .and_then(|line| line.strip_prefix("diff --git "))
            .and_then(|line| line.split_once(" "))
    {
        old_path = diff_path(old);
        new_path = diff_path(new);
    }
    if matches!(status, DiffFileStatus::Added) {
        old_path = None;
    }
    if matches!(status, DiffFileStatus::Deleted) {
        new_path = None;
    }
    let is_binary = patch.contains("GIT binary patch") || patch.contains("Binary files ");
    DiffFile {
        repository_id: repository_id.into(),
        old_path,
        new_path,
        old_blob_sha: None,
        new_blob_sha: None,
        status,
        is_binary,
        patch: patch.into(),
        hunks: parse_hunks(repository_id, patch),
    }
}

fn diff_path(value: &str) -> Option<String> {
    let value = value.split('\t').next().unwrap_or(value);
    if value == "/dev/null" {
        None
    } else {
        Some(
            value
                .strip_prefix("a/")
                .or_else(|| value.strip_prefix("b/"))
                .unwrap_or(value)
                .into(),
        )
    }
}

fn parse_hunks(repository_id: &str, patch: &str) -> Vec<DiffHunk> {
    let mut hunks = Vec::new();
    let mut current: Option<DiffHunk> = None;
    for line in patch.lines() {
        if let Some((old_start, old_lines, new_start, new_lines, header)) = parse_hunk_header(line)
        {
            if let Some(hunk) = current.take() {
                hunks.push(hunk);
            }
            current = Some(DiffHunk {
                repository_id: repository_id.into(),
                old_start,
                old_lines,
                new_start,
                new_lines,
                header,
                lines: Vec::new(),
            });
        } else if let Some(hunk) = current.as_mut() {
            let (kind, content) = match line.as_bytes().first() {
                Some(b' ') => (Some(DiffLineKind::Context), &line[1..]),
                Some(b'+') => (Some(DiffLineKind::Addition), &line[1..]),
                Some(b'-') => (Some(DiffLineKind::Deletion), &line[1..]),
                _ => (None, ""),
            };
            if let Some(kind) = kind {
                hunk.lines.push(DiffLine {
                    kind,
                    content: content.into(),
                });
            }
        }
    }
    if let Some(hunk) = current {
        hunks.push(hunk);
    }
    hunks
}

fn parse_hunk_header(line: &str) -> Option<(u32, u32, u32, u32, String)> {
    if !line.starts_with("@@ ") {
        return None;
    }
    let (ranges, tail) = line[3..].split_once(" @@")?;
    let mut pieces = ranges.split_whitespace();
    let old = parse_range(pieces.next()?.strip_prefix('-')?)?;
    let new = parse_range(pieces.next()?.strip_prefix('+')?)?;
    Some((old.0, old.1, new.0, new.1, tail.trim_start().into()))
}

fn parse_range(value: &str) -> Option<(u32, u32)> {
    match value.split_once(',') {
        Some((start, count)) => Some((start.parse().ok()?, count.parse().ok()?)),
        None => Some((value.parse().ok()?, 1)),
    }
}

#[cfg(test)]
mod tests {
    use super::{DiffFileStatus, materialize_manifest};
    use crate::{RepositorySnapshot, WorkspaceManifest};
    use chrono::Utc;
    use std::{fs, path::Path, process::Command};
    use tempfile::TempDir;

    fn run(root: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().into()
    }
    fn commit(root: &Path, message: &str) -> String {
        run(root, &["add", "-A"]);
        run(root, &["commit", "-m", message]);
        run(root, &["rev-parse", "HEAD"])
    }
    fn repo(root: &Path, shared: &str) -> (String, String) {
        run(root, &["init"]);
        run(root, &["config", "user.name", "Test"]);
        run(root, &["config", "user.email", "test@example.com"]);
        fs::write(root.join("same.txt"), format!("old {shared}\n")).unwrap();
        fs::write(root.join("gone.txt"), "gone\n").unwrap();
        fs::write(root.join("blob.bin"), [0_u8; 1024]).unwrap();
        let base = commit(root, "base");
        fs::write(root.join("same.txt"), format!("new {shared}\n")).unwrap();
        fs::remove_file(root.join("gone.txt")).unwrap();
        fs::write(root.join("added.txt"), "added\n").unwrap();
        fs::write(root.join("blob.bin"), [9_u8; 1024]).unwrap();
        let head = commit(root, "head");
        (base, head)
    }
    #[test]
    fn materializes_pinned_multi_repository_diffs_without_worktree_content() {
        let temp = TempDir::new().unwrap();
        let one = temp.path().join("one");
        let two = temp.path().join("two");
        fs::create_dir_all(&one).unwrap();
        fs::create_dir_all(&two).unwrap();
        let (one_base, one_head) = repo(&one, "one");
        let (two_base, two_head) = repo(&two, "two");
        // This uncommitted change proves materialization reads only the pinned objects.
        fs::write(one.join("same.txt"), "uncommitted\n").unwrap();
        let manifest = WorkspaceManifest {
            workspace_id: "w".into(),
            workspace_root: temp.path().display().to_string(),
            topic: "t".into(),
            before_fingerprint: "before".into(),
            after_fingerprint: "after".into(),
            created_at: Utc::now(),
            repositories: vec![
                RepositorySnapshot {
                    repository_id: "one".into(),
                    root: "one".into(),
                    branch: "main".into(),
                    base_sha: one_base,
                    head_sha: one_head,
                    remote_fingerprint: None,
                    object_checksum: String::new(),
                    capture_metadata: None,
                },
                RepositorySnapshot {
                    repository_id: "two".into(),
                    root: "two".into(),
                    branch: "main".into(),
                    base_sha: two_base,
                    head_sha: two_head,
                    remote_fingerprint: None,
                    object_checksum: String::new(),
                    capture_metadata: None,
                },
            ],
        };
        let result = materialize_manifest(&manifest).unwrap();
        assert_eq!(
            result
                .repositories
                .iter()
                .map(|r| r.repository_id.as_str())
                .collect::<Vec<_>>(),
            ["one", "two"]
        );
        for repo in result.repositories {
            assert_eq!(repo.files.len(), 4);
            let same = repo
                .files
                .iter()
                .find(|file| file.new_path.as_deref() == Some("same.txt"))
                .unwrap();
            assert_eq!(same.repository_id, repo.repository_id);
            assert!(same.patch.contains("new"));
            assert_eq!(same.hunks[0].repository_id, repo.repository_id);
            assert!(
                repo.files
                    .iter()
                    .any(|file| file.status == DiffFileStatus::Added
                        && file.new_path.as_deref() == Some("added.txt"))
            );
            assert!(
                repo.files
                    .iter()
                    .any(|file| file.status == DiffFileStatus::Deleted
                        && file.old_path.as_deref() == Some("gone.txt"))
            );
            assert!(
                repo.files
                    .iter()
                    .any(|file| file.is_binary && file.new_path.as_deref() == Some("blob.bin"))
            );
        }
    }
}
