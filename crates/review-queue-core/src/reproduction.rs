//! Explicit, local-only reconstruction of a pinned review workspace.
//!
//! A preview is deliberately pure: it calculates destinations and a shell
//! bundle but neither creates directories nor invokes Git.  Materialization
//! repeats validation immediately before it writes, then clones every local
//! source repository and checks out its captured commit detached.

use std::{
    collections::BTreeSet,
    fs,
    path::{Component, Path, PathBuf},
    process::Command,
};

use serde::{Deserialize, Serialize};

use crate::{DomainError, RepositoryCaptureMetadata, RepositorySnapshot, WorkspaceManifest};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ReproductionRepository {
    pub repository_id: String,
    pub source: String,
    pub destination: String,
    pub head_sha: String,
    /// Expected tree identity verified before any destination is created.
    #[serde(default)]
    pub object_checksum: String,
    /// Capture inventory, exclusions, diagnostics, and declarative recipe are
    /// surfaced in preview instead of being hidden in the stored manifest.
    #[serde(default)]
    pub capture_metadata: Option<Box<RepositoryCaptureMetadata>>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ReproductionPreview {
    pub destination: String,
    pub repositories: Vec<ReproductionRepository>,
    /// A POSIX-shell-safe, copyable reconstruction script. It intentionally
    /// contains no credentials or remote URLs.
    pub command_bundle: String,
    /// Working directory in which the user should start a fresh agent.
    pub agent_working_directory: String,
    /// Explicitly manual launch guidance. The bundle reconstructs and enters
    /// the environment but never starts or prompts an agent.
    pub launch_guidance: String,
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ReproductionResult {
    pub destination: String,
    pub repositories: Vec<ReproductionRepository>,
    pub command_bundle: String,
    pub agent_working_directory: String,
    pub launch_guidance: String,
    #[serde(default)]
    pub warnings: Vec<String>,
}

/// Computes a reconstruction plan without touching either source or target.
pub fn preview(
    manifest: &WorkspaceManifest,
    destination: impl AsRef<Path>,
) -> Result<ReproductionPreview, DomainError> {
    let destination = absolute(destination.as_ref())?;
    let repositories = plan_repositories(manifest, &destination)?;
    let warnings = repositories
        .iter()
        .flat_map(|repository| {
            repository
                .capture_metadata
                .as_deref()
                .into_iter()
                .flat_map(|metadata| metadata.warnings.iter().cloned())
        })
        .collect();
    Ok(ReproductionPreview {
        destination: destination.to_string_lossy().into_owned(),
        command_bundle: command_bundle(&destination, &repositories),
        agent_working_directory: destination.to_string_lossy().into_owned(),
        launch_guidance: "After the setup bundle completes, start a fresh agent session in this working directory, then submit the prepared feedback prompt manually.".into(),
        repositories,
        warnings,
    })
}

/// Reconstructs the pinned commits after the caller has explicitly confirmed
/// the preview. This function has no Store or delivery dependency.
pub fn materialize(
    manifest: &WorkspaceManifest,
    destination: impl AsRef<Path>,
    confirmed: bool,
) -> Result<ReproductionResult, DomainError> {
    if !confirmed {
        return Err(error(
            "Reproduction requires explicit confirmation.",
            "No directories, source files, or Git refs were changed.",
            "Review the preview, then confirm materialization.",
            "reproduction_confirmation_required",
        ));
    }
    let preview = preview(manifest, destination)?;
    ensure_destination_clean(Path::new(&preview.destination))?;
    // Verify all source objects before creating the destination, so malformed
    // manifests do not leave a partial reconstruction behind.
    for repository in &preview.repositories {
        verify_source(repository)?;
    }
    fs::create_dir_all(&preview.destination).map_err(|e| {
        fs_error(
            Path::new(&preview.destination),
            e,
            "Could not create the reproduction destination.",
        )
    })?;
    for repository in &preview.repositories {
        let destination = Path::new(&repository.destination);
        let parent = destination
            .parent()
            .expect("planned destinations have parents");
        fs::create_dir_all(parent).map_err(|e| {
            fs_error(
                parent,
                e,
                "Could not create a repository destination directory.",
            )
        })?;
        run_git(
            None,
            [
                "clone",
                "--no-checkout",
                "--no-local",
                "--",
                &repository.source,
                &repository.destination,
            ],
            destination,
        )?;
        run_git(
            Some(destination),
            ["checkout", "--detach", "--force", &repository.head_sha],
            destination,
        )?;
    }
    Ok(ReproductionResult {
        destination: preview.destination,
        repositories: preview.repositories,
        command_bundle: preview.command_bundle,
        agent_working_directory: preview.agent_working_directory,
        launch_guidance: preview.launch_guidance,
        warnings: preview.warnings,
    })
}

fn plan_repositories(
    manifest: &WorkspaceManifest,
    destination: &Path,
) -> Result<Vec<ReproductionRepository>, DomainError> {
    if manifest.repositories.is_empty() {
        return Err(error(
            "The review manifest has no repositories to reproduce.",
            "No directories, source files, or Git refs were changed.",
            "Capture a workspace with at least one Git repository and retry.",
            "reproduction_repositories_required",
        ));
    }
    let workspace_root = absolute(Path::new(&manifest.workspace_root))?;
    let mut ids = BTreeSet::new();
    let mut destinations = BTreeSet::new();
    let mut result = Vec::with_capacity(manifest.repositories.len());
    for snapshot in &manifest.repositories {
        validate_snapshot(
            snapshot,
            &workspace_root,
            destination,
            &mut ids,
            &mut destinations,
        )?;
        let relative = repository_relative_path(snapshot, &workspace_root)?;
        let target = destination.join(relative);
        result.push(ReproductionRepository {
            repository_id: snapshot.repository_id.clone(),
            source: source_path(snapshot, &workspace_root)?
                .to_string_lossy()
                .into_owned(),
            destination: target.to_string_lossy().into_owned(),
            head_sha: snapshot.head_sha.clone(),
            object_checksum: snapshot.object_checksum.clone(),
            capture_metadata: snapshot.capture_metadata.clone(),
        });
    }
    result.sort_by(|a, b| a.repository_id.cmp(&b.repository_id));
    Ok(result)
}

fn validate_snapshot(
    snapshot: &RepositorySnapshot,
    workspace_root: &Path,
    destination: &Path,
    ids: &mut BTreeSet<String>,
    destinations: &mut BTreeSet<PathBuf>,
) -> Result<(), DomainError> {
    if snapshot.repository_id.trim().is_empty() || !ids.insert(snapshot.repository_id.clone()) {
        return Err(error(
            "The review manifest contains a duplicate or empty repository ID.",
            "No directories, source files, or Git refs were changed.",
            "Capture the workspace again before reproducing it.",
            "reproduction_invalid_manifest",
        ));
    }
    if snapshot.head_sha.trim().is_empty() || snapshot.head_sha.chars().any(char::is_whitespace) {
        return Err(error(
            "The review manifest contains an invalid saved commit.",
            "No directories, source files, or Git refs were changed.",
            "Capture the workspace again before reproducing it.",
            "reproduction_invalid_manifest",
        ));
    }
    validate_capture_metadata(snapshot)?;
    let relative = repository_relative_path(snapshot, workspace_root)?;
    let target = destination.join(relative);
    if !destinations.insert(target) {
        return Err(error(
            "The review manifest maps multiple repositories to one destination.",
            "No directories, source files, or Git refs were changed.",
            "Capture the workspace again before reproducing it.",
            "reproduction_invalid_manifest",
        ));
    }
    Ok(())
}

fn validate_capture_metadata(snapshot: &RepositorySnapshot) -> Result<(), DomainError> {
    let Some(metadata) = snapshot.capture_metadata.as_deref() else {
        return Ok(());
    };
    let checksums = &metadata.object_checksums;
    let recipe = &metadata.materialization;
    let invalid = metadata.base_ref.trim().is_empty()
        || checksums.base_commit != snapshot.base_sha
        || checksums.head_commit != snapshot.head_sha
        || (!snapshot.object_checksum.is_empty()
            && checksums.head_tree != snapshot.object_checksum)
        || recipe.schema_version == 0
        || recipe.source_repository_root != snapshot.root
        || recipe.required_commit != snapshot.head_sha
        || recipe.required_tree != checksums.head_tree
        || recipe.object_format != checksums.object_format
        || !recipe.checkout_detached;
    if invalid {
        return Err(error(
            "The review manifest contains an inconsistent materialization recipe.",
            "No directories, source files, or Git refs were changed.",
            "Capture the workspace again before reproducing it.",
            "reproduction_invalid_manifest",
        ));
    }
    Ok(())
}

fn repository_relative_path(
    snapshot: &RepositorySnapshot,
    workspace_root: &Path,
) -> Result<PathBuf, DomainError> {
    let source = source_path(snapshot, workspace_root)?;
    let relative = source.strip_prefix(workspace_root).map_err(|_| {
        error(
            "A repository in the review manifest is outside its workspace root.",
            "No directories, source files, or Git refs were changed.",
            "Capture the workspace again before reproducing it.",
            "reproduction_repository_outside_workspace",
        )
    })?;
    if relative.components().any(|c| {
        matches!(
            c,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return Err(error(
            "The review manifest contains an unsafe repository path.",
            "No directories, source files, or Git refs were changed.",
            "Capture the workspace again before reproducing it.",
            "reproduction_invalid_manifest",
        ));
    }
    Ok(relative.to_owned())
}

/// Capture stores repository roots relative to `workspace_root`; accepting an
/// absolute root keeps manually imported legacy manifests usable as well.
fn source_path(
    snapshot: &RepositorySnapshot,
    workspace_root: &Path,
) -> Result<PathBuf, DomainError> {
    let raw = Path::new(&snapshot.root);
    let source = if raw.is_absolute() {
        raw.to_owned()
    } else {
        workspace_root.join(raw)
    };
    if raw
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(error(
            "The review manifest contains an unsafe repository path.",
            "No directories, source files, or Git refs were changed.",
            "Capture the workspace again before reproducing it.",
            "reproduction_invalid_manifest",
        ));
    }
    Ok(source)
}

fn ensure_destination_clean(destination: &Path) -> Result<(), DomainError> {
    match fs::read_dir(destination) {
        Ok(mut entries) => {
            if entries.next().is_some() {
                Err(error(
                    &format!(
                        "Reproduction destination '{}' is not empty.",
                        destination.display()
                    ),
                    "No files in the destination or source workspace were changed.",
                    "Choose a new or empty destination directory and retry.",
                    "reproduction_destination_not_empty",
                ))
            } else {
                Ok(())
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(fs_error(
            destination,
            e,
            "Could not inspect the reproduction destination.",
        )),
    }
}

fn verify_source(repository: &ReproductionRepository) -> Result<(), DomainError> {
    let source = Path::new(&repository.source);
    run_git(Some(source), ["rev-parse", "--is-inside-work-tree"], source)?;
    run_git(
        Some(source),
        [
            "cat-file",
            "-e",
            &format!("{}^{{commit}}", repository.head_sha),
        ],
        source,
    )?;
    if !repository.object_checksum.trim().is_empty() {
        let actual_tree = git_stdout(
            Some(source),
            ["rev-parse", &format!("{}^{{tree}}", repository.head_sha)],
            source,
        )?;
        if actual_tree != repository.object_checksum {
            return Err(error(
                &format!(
                    "The saved tree for repository '{}' no longer matches its manifest checksum.",
                    repository.repository_id
                ),
                "No reproduction destination was created and the source repository was unchanged.",
                "Restore the captured Git objects or capture the workspace again.",
                "reproduction_object_mismatch",
            ));
        }
    }
    Ok(())
}

fn git_stdout<const N: usize>(
    cwd: Option<&Path>,
    args: [&str; N],
    context: &Path,
) -> Result<String, DomainError> {
    let mut command = Command::new("git");
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    let output = command
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .args(args)
        .output()
        .map_err(|e| {
            error(
                &format!("Could not run Git for '{}': {e}", context.display()),
                "No source files or Git refs were changed.",
                "Install Git or repair the local repository, then retry.",
                "git_unavailable",
            )
        })?;
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned());
    }
    let detail = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    Err(error(
        &format!(
            "Git could not reproduce '{}': {}",
            context.display(),
            if detail.is_empty() {
                "unknown Git error"
            } else {
                &detail
            }
        ),
        "Source files and source Git refs were not changed.",
        "Restore the captured Git objects and retry with an empty destination.",
        "reproduction_git_failed",
    ))
}

fn run_git<const N: usize>(
    cwd: Option<&Path>,
    args: [&str; N],
    context: &Path,
) -> Result<(), DomainError> {
    let mut command = Command::new("git");
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    let output = command
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .args(args)
        .output()
        .map_err(|e| {
            error(
                &format!("Could not run Git for '{}': {e}", context.display()),
                "No source files or Git refs were changed.",
                "Install Git or repair the local repository, then retry.",
                "git_unavailable",
            )
        })?;
    if output.status.success() {
        return Ok(());
    }
    let detail = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    Err(error(
        &format!(
            "Git could not reproduce '{}': {}",
            context.display(),
            if detail.is_empty() {
                "unknown Git error"
            } else {
                &detail
            }
        ),
        "Source files and source Git refs were not changed. The reproduction destination may contain partial files.",
        "Fix the Git error, choose a new empty destination, and retry.",
        "reproduction_git_failed",
    ))
}

fn command_bundle(destination: &Path, repositories: &[ReproductionRepository]) -> String {
    let mut lines = vec![format!(
        "mkdir -p {}",
        shell_quote(&destination.to_string_lossy())
    )];
    for repository in repositories {
        lines.push(format!(
            "mkdir -p {}",
            shell_quote(
                &Path::new(&repository.destination)
                    .parent()
                    .expect("planned destination has parent")
                    .to_string_lossy()
            )
        ));
        lines.push(format!(
            "git clone --no-checkout --no-local -- {} {}",
            shell_quote(&repository.source),
            shell_quote(&repository.destination)
        ));
        lines.push(format!(
            "git -C {} checkout --detach --force {}",
            shell_quote(&repository.destination),
            shell_quote(&repository.head_sha)
        ));
    }
    lines.push(format!(
        "cd {}",
        shell_quote(&destination.to_string_lossy())
    ));
    lines.join("\n")
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\\"'\\\"'"))
}

fn absolute(path: &Path) -> Result<PathBuf, DomainError> {
    if path.is_absolute() {
        Ok(path.to_owned())
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .map_err(|e| {
                error(
                    &format!("Could not resolve path '{}': {e}", path.display()),
                    "No files were changed.",
                    "Retry from a valid working directory.",
                    "reproduction_path_unavailable",
                )
            })
    }
}

fn fs_error(path: &Path, e: impl std::fmt::Display, what: &str) -> DomainError {
    error(
        &format!("{what} '{}': {e}", path.display()),
        "No source files or Git refs were changed.",
        "Choose a writable empty destination and retry.",
        "reproduction_destination_unavailable",
    )
}
fn error(what: &str, safety: &str, next: &str, code: &str) -> DomainError {
    DomainError::actionable(what, safety, next, code)
}

#[cfg(test)]
mod tests {
    use super::{materialize, preview};
    use crate::{
        RepositoryCaptureMetadata, RepositoryExclusion, RepositoryInclusions,
        RepositoryMaterializationRecipe, RepositoryObjectChecksums, RepositorySnapshot,
        WorkspaceManifest,
    };
    use chrono::Utc;
    use std::{fs, path::Path, process::Command};
    use tempfile::TempDir;

    fn git(path: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .current_dir(path)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().into()
    }
    fn repository(path: &Path, file: &str) -> String {
        fs::create_dir_all(path).unwrap();
        git(path, &["init", "-q"]);
        git(path, &["config", "user.email", "test@example.invalid"]);
        git(path, &["config", "user.name", "Test"]);
        fs::write(path.join(file), format!("{file}\n")).unwrap();
        git(path, &["add", "."]);
        git(path, &["commit", "-qm", "captured"]);
        git(path, &["rev-parse", "HEAD"])
    }
    fn manifest(workspace: &Path, repos: Vec<(&str, String)>) -> WorkspaceManifest {
        WorkspaceManifest {
            workspace_id: "workspace".into(),
            workspace_root: workspace.display().to_string(),
            topic: "topic".into(),
            repositories: repos
                .into_iter()
                .map(|(relative, sha)| {
                    let root = workspace.join(relative);
                    let tree = git(&root, &["rev-parse", &format!("{sha}^{{tree}}")]);
                    RepositorySnapshot {
                        repository_id: if relative.is_empty() {
                            ".".into()
                        } else {
                            relative.into()
                        },
                        // This is intentionally relative: `capture::snapshot`
                        // stores manifest roots in exactly this portable form.
                        root: if relative.is_empty() { "." } else { relative }.into(),
                        branch: "main".into(),
                        base_sha: sha.clone(),
                        head_sha: sha,
                        remote_fingerprint: None,
                        object_checksum: tree,
                        capture_metadata: None,
                    }
                })
                .collect(),
            before_fingerprint: "before".into(),
            after_fingerprint: "after".into(),
            created_at: Utc::now(),
        }
    }
    #[test]
    fn preview_and_cancel_never_write() {
        let temp = TempDir::new().unwrap();
        let workspace = temp.path().join("workspace");
        let sha = repository(&workspace.join("one"), "one.txt");
        let target = temp.path().join("reproduction");
        let plan = preview(&manifest(&workspace, vec![("one", sha)]), &target).unwrap();
        assert!(!target.exists());
        assert!(
            plan.command_bundle
                .contains("git clone --no-checkout --no-local")
        );
        assert!(plan.command_bundle.contains("\ncd "));
        assert_eq!(
            plan.agent_working_directory,
            target.to_string_lossy().as_ref()
        );
        assert!(plan.launch_guidance.contains("start a fresh agent session"));
        let error = materialize(
            &manifest(
                &workspace,
                vec![("one", git(&workspace.join("one"), &["rev-parse", "HEAD"]))],
            ),
            &target,
            false,
        )
        .unwrap_err();
        assert_eq!(error.error.code, "reproduction_confirmation_required");
        assert!(!target.exists());
    }
    #[test]
    fn confirmed_materialization_restores_two_repositories_at_saved_heads() {
        let temp = TempDir::new().unwrap();
        let workspace = temp.path().join("workspace");
        let one = repository(&workspace.join("services/one"), "one.txt");
        let two = repository(&workspace.join("tools/two"), "two.txt");
        let source_one = workspace.join("services/one");
        let before_ref = git(&source_one, &["rev-parse", "HEAD"]);
        let target = temp.path().join("reproduction");
        let result = materialize(
            &manifest(
                &workspace,
                vec![("services/one", one.clone()), ("tools/two", two.clone())],
            ),
            &target,
            true,
        )
        .unwrap();
        assert_eq!(result.repositories.len(), 2);
        assert_eq!(
            git(&target.join("services/one"), &["rev-parse", "HEAD"]),
            one
        );
        assert_eq!(git(&target.join("tools/two"), &["rev-parse", "HEAD"]), two);
        assert_eq!(git(&source_one, &["rev-parse", "HEAD"]), before_ref);
        let detached = Command::new("git")
            .current_dir(target.join("services/one"))
            .args(["symbolic-ref", "-q", "--short", "HEAD"])
            .status()
            .unwrap();
        assert!(!detached.success());
    }

    #[test]
    fn preview_surfaces_capture_diagnostics_and_materialization_verifies_objects() {
        let temp = TempDir::new().unwrap();
        let workspace = temp.path().join("workspace");
        let sha = repository(&workspace.join("one"), "one.txt");
        let mut manifest = manifest(&workspace, vec![("one", sha.clone())]);
        let snapshot = &mut manifest.repositories[0];
        let tree = snapshot.object_checksum.clone();
        let object_format = git(
            &workspace.join("one"),
            &["rev-parse", "--show-object-format"],
        );
        snapshot.capture_metadata = Some(Box::new(RepositoryCaptureMetadata {
            base_ref: "refs/heads/main".into(),
            inclusions: RepositoryInclusions {
                tracked_paths: vec!["one.txt".into()],
                untracked_paths: vec!["new.txt".into()],
                deleted_paths: vec!["old.txt".into()],
                binary_paths: vec!["asset.bin".into()],
            },
            exclusions: vec![RepositoryExclusion {
                path: "ignored.log".into(),
                reason: "git_ignored".into(),
            }],
            warnings: vec!["1 Git-ignored path(s) were excluded from capture.".into()],
            object_checksums: RepositoryObjectChecksums {
                object_format: object_format.clone(),
                base_commit: sha.clone(),
                base_tree: tree.clone(),
                head_commit: sha.clone(),
                head_tree: tree.clone(),
            },
            materialization: RepositoryMaterializationRecipe {
                schema_version: 1,
                source_repository_root: "one".into(),
                required_commit: sha,
                required_tree: tree,
                object_format,
                checkout_detached: true,
            },
        }));
        let target = temp.path().join("preview");
        let preview = preview(&manifest, &target).unwrap();
        let metadata = preview.repositories[0].capture_metadata.as_deref().unwrap();
        assert_eq!(metadata.inclusions.untracked_paths, vec!["new.txt"]);
        assert_eq!(metadata.exclusions[0].path, "ignored.log");
        assert_eq!(preview.warnings, metadata.warnings);

        let mut invalid = manifest;
        invalid.repositories[0].object_checksum = "0000000000000000000000000000000000000000".into();
        let invalid_metadata = invalid.repositories[0].capture_metadata.as_mut().unwrap();
        invalid_metadata.object_checksums.head_tree =
            "0000000000000000000000000000000000000000".into();
        invalid_metadata.materialization.required_tree =
            "0000000000000000000000000000000000000000".into();
        let error = materialize(&invalid, &target, true).unwrap_err();
        assert_eq!(error.error.code, "reproduction_object_mismatch");
        assert!(!target.exists());
    }
    #[test]
    fn rejects_non_empty_destination_without_touching_it() {
        let temp = TempDir::new().unwrap();
        let workspace = temp.path().join("workspace");
        let sha = repository(&workspace.join("one"), "one.txt");
        let target = temp.path().join("reproduction");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("keep"), "keep").unwrap();
        let error =
            materialize(&manifest(&workspace, vec![("one", sha)]), &target, true).unwrap_err();
        assert_eq!(error.error.code, "reproduction_destination_not_empty");
        assert_eq!(fs::read_to_string(target.join("keep")).unwrap(), "keep");
    }
}
