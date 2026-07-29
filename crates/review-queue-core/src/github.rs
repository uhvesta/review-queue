//! Token-free GitHub PR adapter. The desktop shell owns credentials and
//! provides its transport; this module never starts background I/O.

use crate::adapters::{
    GithubPullRequestMetadata, GithubReviewEvent, ImportedComment, PublishCommentDisposition,
    PublishPreview, PublishPreviewComment, StalenessStatus, publish_event_for_decision,
};
use crate::{Decision, DomainError, FormalComment};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use uuid::Uuid;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct GithubPullRequestLocator {
    pub host: String,
    pub owner: String,
    pub repository: String,
    pub pull_number: u64,
}
impl GithubPullRequestLocator {
    pub fn topic_identity(&self) -> String {
        format!(
            "{}/{}/{}#{}",
            self.host, self.owner, self.repository, self.pull_number
        )
    }
}

pub fn parse_pull_request_url(value: &str) -> Result<GithubPullRequestLocator, DomainError> {
    let rest = value
        .trim()
        .strip_prefix("https://")
        .ok_or_else(|| invalid_url("Use an HTTPS pull request URL."))?;
    let rest = &rest[..rest.find(['?', '#']).unwrap_or(rest.len())];
    let (host, path) = rest
        .split_once('/')
        .ok_or_else(|| invalid_url("Include a repository and pull request number."))?;
    if host.is_empty()
        || host.contains('@')
        || host.contains(':')
        || host.chars().any(char::is_whitespace)
    {
        return Err(invalid_url("Use a hostname without credentials or a port."));
    }
    let parts: Vec<_> = path.split('/').filter(|p| !p.is_empty()).collect();
    if parts.len() != 4 || parts[2] != "pull" {
        return Err(invalid_url(
            "Use https://host/owner/repository/pull/NUMBER.",
        ));
    }
    let pull_number = parts[3]
        .parse()
        .ok()
        .filter(|n: &u64| *n > 0)
        .ok_or_else(|| invalid_url("The pull request number must be positive."))?;
    let out = GithubPullRequestLocator {
        host: host.into(),
        owner: parts[0].into(),
        repository: parts[1].into(),
        pull_number,
    };
    token_free(&[&out.host, &out.owner, &out.repository])?;
    Ok(out)
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct GithubMaterializedFile {
    pub path: String,
    pub status: String,
    pub base_blob_sha: String,
    pub head_blob_sha: String,
    pub is_binary: bool,
    #[serde(default)]
    pub base_content: Option<String>,
    #[serde(default)]
    pub head_content: Option<String>,
    #[serde(default)]
    pub base_content_base64: Option<String>,
    #[serde(default)]
    pub head_content_base64: Option<String>,
    pub unified_diff: String,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct GithubQueuePayload {
    pub locator: GithubPullRequestLocator,
    pub metadata: GithubPullRequestMetadata,
    pub source_materialized: bool,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct GithubOpenedPullRequest {
    pub payload: GithubQueuePayload,
    pub files: Vec<GithubMaterializedFile>,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct LocalCommentDraft {
    pub id: String,
    pub thread_id: String,
    pub body: String,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct GithubCommentRefresh {
    pub imported: Vec<ImportedComment>,
    pub local_drafts: Vec<LocalCommentDraft>,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct GithubPublishComment {
    pub formal_comment_id: String,
    pub thread_id: String,
    pub body: String,
    pub disposition: PublishCommentDisposition,
    pub fallback_reference: Option<String>,
    #[serde(default)]
    pub anchor: Option<crate::Anchor>,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct GithubPublishRequest {
    pub idempotency_key: String,
    pub target: GithubPullRequestMetadata,
    pub decision: Decision,
    pub event: GithubReviewEvent,
    pub comments: Vec<GithubPublishComment>,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct GithubPublishReceipt {
    pub review_id: String,
    pub idempotency_key: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct GithubReplyRequest {
    pub idempotency_key: String,
    pub target: GithubPullRequestMetadata,
    pub formal_comment_id: String,
    pub formal_revision: i64,
    pub upstream_comment_id: u64,
    pub body: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct GithubReplyReceipt {
    pub comment_id: String,
    pub idempotency_key: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct GithubReplyAttempt {
    pub id: String,
    pub round_id: String,
    pub request: GithubReplyRequest,
    pub status: GithubPublishStatus,
    #[serde(default)]
    pub comment_id: Option<String>,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GithubPublishStatus {
    Prepared,
    Posting,
    Completed,
    Unknown,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct GithubRoundState {
    pub round_id: String,
    pub payload: GithubQueuePayload,
    #[serde(default)]
    pub files: Vec<GithubMaterializedFile>,
    #[serde(default)]
    pub imported_comments: Vec<ImportedComment>,
    #[serde(default)]
    pub last_staleness: Option<StalenessStatus>,
}

pub fn preview_reproduction(
    manifest: &crate::WorkspaceManifest,
    state: &GithubRoundState,
    destination: impl AsRef<Path>,
) -> Result<crate::machine::MachineReproductionPreview, DomainError> {
    crate::machine::preview_snapshot_reproduction(
        &reproduction_snapshot(manifest, state)?,
        destination,
    )
}

pub fn reproduce(
    manifest: &crate::WorkspaceManifest,
    state: &GithubRoundState,
    destination: impl AsRef<Path>,
) -> Result<crate::machine::MachineReproductionResult, DomainError> {
    crate::machine::reproduce_snapshot(&reproduction_snapshot(manifest, state)?, destination)
}

fn reproduction_snapshot(
    manifest: &crate::WorkspaceManifest,
    state: &GithubRoundState,
) -> Result<crate::machine::MachineSnapshot, DomainError> {
    if !state.payload.source_materialized || state.files.is_empty() {
        return Err(err(
            "GitHub source must be opened and cached before reproduction.",
            "No directory was created and no remote request was made.",
            "Open the pull request source, then preview reproduction again.",
            "github_reproduction_source_not_cached",
        ));
    }
    let repository_id = manifest
        .repositories
        .first()
        .map(|repository| repository.repository_id.clone())
        .ok_or_else(|| {
            err(
                "The GitHub round has no repository manifest.",
                "No directory was created.",
                "Refresh or re-add the pull request.",
                "github_reproduction_manifest_invalid",
            )
        })?;
    Ok(crate::machine::MachineSnapshot {
        source_item_id: state.round_id.clone(),
        snapshot_version: state.payload.metadata.head_sha.clone(),
        manifest: manifest.clone(),
        files: state
            .files
            .iter()
            .map(|file| crate::machine::MachineSnapshotFile {
                repository_id: repository_id.clone(),
                workspace_relative_path: file.path.clone(),
                status: file.status.clone(),
                base_blob_sha: file.base_blob_sha.clone(),
                head_blob_sha: file.head_blob_sha.clone(),
                unified_diff: file.unified_diff.clone(),
                is_binary: file.is_binary,
                base_content_base64: file.base_content_base64.clone(),
                head_content_base64: file.head_content_base64.clone(),
                materialized: None,
            })
            .collect(),
    })
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct GithubPublishAttempt {
    pub id: String,
    pub round_id: String,
    pub preview: PublishPreview,
    pub request: GithubPublishRequest,
    pub status: GithubPublishStatus,
    #[serde(default)]
    pub review_id: Option<String>,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub completed_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub replies: Vec<GithubReplyAttempt>,
}

/// This is deliberately a small, token-free contract. Any OAuth client lives
/// above it in the desktop credential boundary.
pub trait GithubTransport {
    fn resolve_metadata(
        &mut self,
        locator: &GithubPullRequestLocator,
    ) -> Result<GithubPullRequestMetadata, DomainError>;
    fn materialize_files(
        &mut self,
        locator: &GithubPullRequestLocator,
    ) -> Result<Vec<GithubMaterializedFile>, DomainError>;
    fn import_comments(
        &mut self,
        locator: &GithubPullRequestLocator,
    ) -> Result<Vec<ImportedComment>, DomainError>;
    fn publish_review(
        &mut self,
        request: &GithubPublishRequest,
    ) -> Result<GithubPublishReceipt, DomainError>;
    fn publish_reply(
        &mut self,
        request: &GithubReplyRequest,
    ) -> Result<GithubReplyReceipt, DomainError>;
}

pub struct GithubAdapter<T> {
    transport: T,
}
impl<T: GithubTransport> GithubAdapter<T> {
    pub fn new(transport: T) -> Self {
        Self { transport }
    }
    pub fn transport(&self) -> &T {
        &self.transport
    }
    pub fn transport_mut(&mut self) -> &mut T {
        &mut self.transport
    }
    /// Metadata-only queue intake. It cannot fetch source, comments, or write.
    pub fn queue_from_url(&mut self, url: &str) -> Result<GithubQueuePayload, DomainError> {
        let locator = parse_pull_request_url(url)?;
        let metadata = self.transport.resolve_metadata(&locator)?;
        match_identity(&locator, &metadata)?;
        metadata.validate()?;
        Ok(GithubQueuePayload {
            locator,
            metadata,
            source_materialized: false,
        })
    }
    /// Explicit source-opening operation; all blobs and diffs are lazy.
    pub fn open_files(
        &mut self,
        queue: &GithubQueuePayload,
    ) -> Result<GithubOpenedPullRequest, DomainError> {
        let files = self.transport.materialize_files(&queue.locator)?;
        for file in &files {
            validate_file(file)?;
        }
        let mut payload = queue.clone();
        payload.source_materialized = true;
        Ok(GithubOpenedPullRequest { payload, files })
    }
    /// Pull-only refresh preserves every local draft verbatim and cannot write.
    pub fn refresh_comments(
        &mut self,
        queue: &GithubQueuePayload,
        local_drafts: Vec<LocalCommentDraft>,
    ) -> Result<GithubCommentRefresh, DomainError> {
        for draft in &local_drafts {
            validate_draft(draft)?;
        }
        let imported = self.transport.import_comments(&queue.locator)?;
        for c in &imported {
            c.validate()?;
        }
        Ok(GithubCommentRefresh {
            imported,
            local_drafts,
        })
    }
    /// Explicit remote read. Callers must never infer stale state from a UI refresh.
    pub fn check_staleness(
        &mut self,
        queue: &GithubQueuePayload,
        checked_at: DateTime<Utc>,
    ) -> Result<StalenessStatus, DomainError> {
        let metadata = self.transport.resolve_metadata(&queue.locator)?;
        match_identity(&queue.locator, &metadata)?;
        Ok(StalenessStatus {
            pinned_head_sha: queue.metadata.head_sha.clone(),
            observed_head_sha: metadata.head_sha,
            checked_at,
        })
    }
    pub fn preflight_publish(
        &self,
        queue: &GithubQueuePayload,
        decision: Option<Decision>,
        comments: &[FormalComment],
        imported_threads: &BTreeSet<String>,
    ) -> Result<(PublishPreview, GithubPublishRequest), DomainError> {
        let decision = decision.ok_or_else(|| {
            err(
                "A decision is required before publishing.",
                "No GitHub write was made and drafts are preserved.",
                "Choose Approve or Request changes, then review the preview.",
                "decision_required",
            )
        })?;
        let mut preview_comments = vec![];
        let mut publish_comments = vec![];
        for comment in comments {
            validate_formal(comment)?;
            let (disposition, fallback_reference) = disposition(comment, imported_threads);
            preview_comments.push(PublishPreviewComment {
                formal_comment_id: comment.id.clone(),
                disposition,
                fallback_reference: fallback_reference.clone(),
            });
            publish_comments.push(GithubPublishComment {
                formal_comment_id: comment.id.clone(),
                thread_id: comment.thread_id.clone(),
                body: comment.body.clone(),
                disposition,
                fallback_reference,
                anchor: comment.anchor.clone(),
            });
        }
        let preview =
            PublishPreview::from_decision(queue.metadata.clone(), decision, preview_comments);
        preview.validate()?;
        Ok((
            preview,
            GithubPublishRequest {
                idempotency_key: Uuid::new_v4().to_string(),
                target: queue.metadata.clone(),
                decision,
                event: publish_event_for_decision(decision),
                comments: publish_comments,
            },
        ))
    }
    /// The one write path; callers must explicitly accept preflight first.
    pub fn publish(
        &mut self,
        request: &GithubPublishRequest,
    ) -> Result<GithubPublishReceipt, DomainError> {
        validate_request(request)?;
        self.transport.publish_review(request)
    }

    pub fn publish_reply(
        &mut self,
        request: &GithubReplyRequest,
    ) -> Result<GithubReplyReceipt, DomainError> {
        if request.idempotency_key.trim().is_empty()
            || request.formal_comment_id.trim().is_empty()
            || request.formal_revision < 1
            || request.upstream_comment_id == 0
            || request.body.trim().is_empty()
        {
            return Err(err(
                "An upstream reply request is incomplete.",
                "No GitHub write was made and the formal draft is preserved.",
                "Refresh imported comments and prepare publishing again.",
                "github_reply_invalid",
            ));
        }
        self.transport.publish_reply(request)
    }
}

fn disposition(
    comment: &FormalComment,
    imported: &BTreeSet<String>,
) -> (PublishCommentDisposition, Option<String>) {
    if imported.contains(&comment.thread_id) {
        return (PublishCommentDisposition::ReplyToImportedThread, None);
    }
    match &comment.anchor {
        None => (PublishCommentDisposition::ReviewBody, None),
        Some(a)
            if a.start_line > 0 && a.end_line >= a.start_line && !a.blob_sha.trim().is_empty() =>
        {
            (PublishCommentDisposition::Inline, None)
        }
        Some(a) => (
            PublishCommentDisposition::BodyFallback,
            Some(format!(
                "{}:{}-{}",
                a.workspace_relative_path, a.start_line, a.end_line
            )),
        ),
    }
}
fn match_identity(
    l: &GithubPullRequestLocator,
    m: &GithubPullRequestMetadata,
) -> Result<(), DomainError> {
    if l.host != m.host
        || l.owner != m.owner
        || l.repository != m.repository
        || l.pull_number != m.pull_number
    {
        Err(err(
            "GitHub returned metadata for a different pull request.",
            "No queue item was changed.",
            "Reconnect PR read and retry the requested pull request.",
            "github_metadata_identity_mismatch",
        ))
    } else {
        Ok(())
    }
}
fn validate_file(f: &GithubMaterializedFile) -> Result<(), DomainError> {
    if [&f.path, &f.status, &f.base_blob_sha, &f.head_blob_sha]
        .iter()
        .any(|v| v.trim().is_empty())
    {
        return Err(err(
            "A materialized file is missing identity data.",
            "The source cache was not accepted.",
            "Refresh after GitHub returns complete file data.",
            "github_file_required",
        ));
    }
    token_free(
        [
            &f.path,
            &f.status,
            &f.base_blob_sha,
            &f.head_blob_sha,
            &f.unified_diff,
        ]
        .into_iter()
        .map(String::as_str)
        .chain(f.base_content.iter().map(String::as_str))
        .chain(f.head_content.iter().map(String::as_str))
        .chain(f.base_content_base64.iter().map(String::as_str))
        .chain(f.head_content_base64.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .as_slice(),
    )
}
fn validate_draft(d: &LocalCommentDraft) -> Result<(), DomainError> {
    if [&d.id, &d.thread_id, &d.body]
        .iter()
        .any(|v| v.trim().is_empty())
    {
        return Err(err(
            "A local draft is incomplete.",
            "No upstream comment was changed.",
            "Finish the draft before refreshing comments.",
            "local_draft_required",
        ));
    }
    token_free(&[&d.id, &d.thread_id, &d.body])
}
fn validate_formal(c: &FormalComment) -> Result<(), DomainError> {
    if [&c.id, &c.thread_id, &c.body]
        .iter()
        .any(|v| v.trim().is_empty())
    {
        return Err(err(
            "A formal comment is incomplete.",
            "No GitHub write was made and drafts are preserved.",
            "Finish the comment before publishing.",
            "formal_comment_required",
        ));
    }
    token_free(&[&c.id, &c.thread_id, &c.body])
}
fn validate_request(r: &GithubPublishRequest) -> Result<(), DomainError> {
    if r.idempotency_key.trim().is_empty() {
        return Err(err(
            "A publish request needs an idempotency key.",
            "No GitHub write was made.",
            "Rebuild the publish preview.",
            "publish_idempotency_required",
        ));
    }
    let p = PublishPreview {
        target: r.target.clone(),
        decision: r.decision,
        event: r.event,
        comments: r
            .comments
            .iter()
            .map(|c| PublishPreviewComment {
                formal_comment_id: c.formal_comment_id.clone(),
                disposition: c.disposition,
                fallback_reference: c.fallback_reference.clone(),
            })
            .collect(),
    };
    p.validate()?;
    if r.comments.iter().any(|c| c.body.trim().is_empty()) {
        return Err(err(
            "A publish comment is empty.",
            "No GitHub write was made.",
            "Keep a comment body or remove the draft.",
            "publish_comment_body_required",
        ));
    }
    if r.comments
        .iter()
        .any(|c| c.disposition == PublishCommentDisposition::ReplyToImportedThread)
    {
        return Err(err(
            "An imported-thread reply cannot be published as review text.",
            "No GitHub write was made and the formal reply is preserved.",
            "Publish it through the dedicated upstream reply request.",
            "github_reply_requires_reply_endpoint",
        ));
    }
    Ok(())
}
fn token_free(values: &[&str]) -> Result<(), DomainError> {
    for v in values {
        crate::redact_for_diagnostics(v)?;
    }
    Ok(())
}
fn invalid_url(next: &str) -> DomainError {
    err(
        "That is not a supported GitHub pull request URL.",
        "No GitHub request was made.",
        next,
        "github_pr_url_invalid",
    )
}
fn err(what: &str, safety: &str, next: &str, code: &str) -> DomainError {
    DomainError::actionable(what, safety, next, code)
}

/// In-memory fake proving the adapter's I/O boundaries. A duplicate publish
/// idempotency key returns the original receipt without another fake write.
#[derive(Clone, Debug)]
pub struct FakeGithubTransport {
    pub metadata: GithubPullRequestMetadata,
    pub files: Vec<GithubMaterializedFile>,
    pub comments: Vec<ImportedComment>,
    pub metadata_reads: usize,
    pub file_reads: usize,
    pub comment_reads: usize,
    pub publish_writes: usize,
    pub reply_writes: usize,
    published: BTreeMap<String, GithubPublishReceipt>,
}
impl FakeGithubTransport {
    pub fn new(metadata: GithubPullRequestMetadata) -> Self {
        Self {
            metadata,
            files: vec![],
            comments: vec![],
            metadata_reads: 0,
            file_reads: 0,
            comment_reads: 0,
            publish_writes: 0,
            reply_writes: 0,
            published: BTreeMap::new(),
        }
    }
    pub fn total_reads(&self) -> usize {
        self.metadata_reads + self.file_reads + self.comment_reads
    }
}
impl GithubTransport for FakeGithubTransport {
    fn resolve_metadata(
        &mut self,
        _: &GithubPullRequestLocator,
    ) -> Result<GithubPullRequestMetadata, DomainError> {
        self.metadata_reads += 1;
        Ok(self.metadata.clone())
    }
    fn materialize_files(
        &mut self,
        _: &GithubPullRequestLocator,
    ) -> Result<Vec<GithubMaterializedFile>, DomainError> {
        self.file_reads += 1;
        Ok(self.files.clone())
    }
    fn import_comments(
        &mut self,
        _: &GithubPullRequestLocator,
    ) -> Result<Vec<ImportedComment>, DomainError> {
        self.comment_reads += 1;
        Ok(self.comments.clone())
    }
    fn publish_review(
        &mut self,
        request: &GithubPublishRequest,
    ) -> Result<GithubPublishReceipt, DomainError> {
        if let Some(r) = self.published.get(&request.idempotency_key) {
            return Ok(r.clone());
        }
        self.publish_writes += 1;
        let r = GithubPublishReceipt {
            review_id: format!("fake-review-{}", self.publish_writes),
            idempotency_key: request.idempotency_key.clone(),
        };
        self.published
            .insert(request.idempotency_key.clone(), r.clone());
        Ok(r)
    }
    fn publish_reply(
        &mut self,
        request: &GithubReplyRequest,
    ) -> Result<GithubReplyReceipt, DomainError> {
        self.reply_writes += 1;
        Ok(GithubReplyReceipt {
            comment_id: format!("fake-comment-{}", self.reply_writes),
            idempotency_key: request.idempotency_key.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Anchor, adapters::GithubPullRequestState};
    use chrono::Utc;
    fn metadata() -> GithubPullRequestMetadata {
        GithubPullRequestMetadata {
            host: "github.example.test".into(),
            owner: "octo".into(),
            repository: "queue".into(),
            pull_number: 42,
            title: "Improve queue".into(),
            body: "description".into(),
            base_sha: "base".into(),
            head_sha: "head-a".into(),
            state: GithubPullRequestState::Open,
            is_draft: false,
            web_url: None,
        }
    }
    fn adapter() -> GithubAdapter<FakeGithubTransport> {
        GithubAdapter::new(FakeGithubTransport::new(metadata()))
    }
    fn payload(a: &mut GithubAdapter<FakeGithubTransport>) -> GithubQueuePayload {
        a.queue_from_url("https://github.example.test/octo/queue/pull/42?x=1")
            .unwrap()
    }
    fn formal(anchor: Option<Anchor>) -> FormalComment {
        FormalComment {
            id: "formal".into(),
            thread_id: "local".into(),
            body: "Please adjust this.".into(),
            anchor,
            revision: 1,
            delivered_revision: None,
        }
    }
    #[test]
    fn parses_only_safe_pr_urls() {
        assert_eq!(
            parse_pull_request_url("http://github.com/o/r/pull/1")
                .unwrap_err()
                .error
                .code,
            "github_pr_url_invalid"
        );
        assert_eq!(
            parse_pull_request_url("https://x@github.com/o/r/pull/1")
                .unwrap_err()
                .error
                .code,
            "github_pr_url_invalid"
        );
        assert_eq!(
            parse_pull_request_url("https://github.com/o/r/issues/1")
                .unwrap_err()
                .error
                .code,
            "github_pr_url_invalid"
        );
        assert_eq!(
            parse_pull_request_url("https://github.com/o/r/pull/7/")
                .unwrap()
                .pull_number,
            7
        );
    }
    #[test]
    fn intake_is_metadata_only_and_open_is_explicit() {
        let mut a = adapter();
        a.transport_mut().files.push(GithubMaterializedFile {
            path: "src/a.rs".into(),
            status: "modified".into(),
            base_blob_sha: "b".into(),
            head_blob_sha: "h".into(),
            is_binary: false,
            base_content: Some("old".into()),
            head_content: Some("new".into()),
            base_content_base64: Some("b2xk".into()),
            head_content_base64: Some("bmV3".into()),
            unified_diff: "-old\n+new".into(),
        });
        let q = payload(&mut a);
        assert!(!q.source_materialized);
        assert_eq!(
            (
                a.transport().metadata_reads,
                a.transport().file_reads,
                a.transport().comment_reads,
                a.transport().publish_writes
            ),
            (1, 0, 0, 0)
        );
        assert!(a.open_files(&q).unwrap().payload.source_materialized);
        assert_eq!(a.transport().file_reads, 1);
    }
    #[test]
    fn refresh_is_pull_only_and_staleness_is_explicit() {
        let mut a = adapter();
        let q = payload(&mut a);
        let d = LocalCommentDraft {
            id: "d".into(),
            thread_id: "t".into(),
            body: "draft".into(),
        };
        assert_eq!(
            a.refresh_comments(&q, vec![d.clone()])
                .unwrap()
                .local_drafts,
            vec![d]
        );
        assert_eq!(a.transport().publish_writes, 0);
        a.transport_mut().metadata.head_sha = "head-b".into();
        assert!(a.check_staleness(&q, Utc::now()).unwrap().is_stale());
        assert_eq!(a.transport().metadata_reads, 2);
    }
    #[test]
    fn preflight_requires_decision_discloses_fallback_and_publish_is_idempotent() {
        let mut a = adapter();
        let q = payload(&mut a);
        assert_eq!(
            a.preflight_publish(&q, None, &[formal(None)], &BTreeSet::new())
                .unwrap_err()
                .error
                .code,
            "decision_required"
        );
        let bad = Anchor {
            repository_id: "r".into(),
            workspace_relative_path: "src/a.rs".into(),
            side: "right".into(),
            start_line: 0,
            end_line: 0,
            blob_sha: "".into(),
            selected_code: "x".into(),
        };
        let (preview, request) = a
            .preflight_publish(
                &q,
                Some(Decision::RequestChanges),
                &[formal(Some(bad))],
                &BTreeSet::new(),
            )
            .unwrap();
        assert_eq!(preview.event, GithubReviewEvent::RequestChanges);
        assert_eq!(
            preview.comments[0].disposition,
            PublishCommentDisposition::BodyFallback
        );
        assert_eq!(
            preview.comments[0].fallback_reference.as_deref(),
            Some("src/a.rs:0-0")
        );
        let first = a.publish(&request).unwrap();
        assert_eq!(first, a.publish(&request).unwrap());
        assert_eq!(a.transport().publish_writes, 1);
        assert_eq!(a.transport().total_reads(), 1);
    }
    #[test]
    fn imported_thread_and_anchor_map_to_correct_dispositions() {
        let mut a = adapter();
        let q = payload(&mut a);
        let anchor = Anchor {
            repository_id: "r".into(),
            workspace_relative_path: "src/a.rs".into(),
            side: "right".into(),
            start_line: 3,
            end_line: 3,
            blob_sha: "blob".into(),
            selected_code: "x".into(),
        };
        let (_, r) = a
            .preflight_publish(
                &q,
                Some(Decision::Approve),
                &[formal(Some(anchor))],
                &BTreeSet::new(),
            )
            .unwrap();
        assert_eq!(r.comments[0].disposition, PublishCommentDisposition::Inline);
        let mut imported = BTreeSet::new();
        imported.insert("local".into());
        let (_, r) = a
            .preflight_publish(&q, Some(Decision::Approve), &[formal(None)], &imported)
            .unwrap();
        assert_eq!(
            r.comments[0].disposition,
            PublishCommentDisposition::ReplyToImportedThread
        );
        let error = a.publish(&r).unwrap_err();
        assert_eq!(error.error.code, "github_reply_requires_reply_endpoint");
        assert_eq!(a.transport().publish_writes, 0);
    }

    #[test]
    fn cached_github_reproduction_is_binary_safe_and_requires_clean_destination() {
        let temporary = tempfile::tempdir().unwrap();
        let destination = temporary.path().join("reproduced");
        let state = GithubRoundState {
            round_id: "round".into(),
            payload: GithubQueuePayload {
                locator: GithubPullRequestLocator {
                    host: "github.com".into(),
                    owner: "o".into(),
                    repository: "r".into(),
                    pull_number: 1,
                },
                metadata: metadata(),
                source_materialized: true,
            },
            files: vec![
                GithubMaterializedFile {
                    path: "text.txt".into(),
                    status: "modified".into(),
                    base_blob_sha: "base".into(),
                    head_blob_sha: "head".into(),
                    is_binary: false,
                    base_content: Some("old".into()),
                    head_content: Some("new".into()),
                    base_content_base64: Some("b2xk".into()),
                    head_content_base64: Some("bmV3".into()),
                    unified_diff: "-old\n+new".into(),
                },
                GithubMaterializedFile {
                    path: "image.bin".into(),
                    status: "added".into(),
                    base_blob_sha: "0000".into(),
                    head_blob_sha: "binary".into(),
                    is_binary: true,
                    base_content: Some(String::new()),
                    head_content: None,
                    base_content_base64: None,
                    head_content_base64: Some("/wA=".into()),
                    unified_diff: "Binary file changed".into(),
                },
            ],
            imported_comments: Vec::new(),
            last_staleness: None,
        };
        let manifest = crate::WorkspaceManifest {
            workspace_id: "github".into(),
            workspace_root: "https://github.com/o/r/pull/1".into(),
            topic: "PR #1".into(),
            repositories: vec![crate::RepositorySnapshot {
                repository_id: "o/r".into(),
                root: "repo".into(),
                branch: "pull/1".into(),
                base_sha: "base".into(),
                head_sha: "head".into(),
                remote_fingerprint: None,
                object_checksum: "head".into(),
            }],
            before_fingerprint: "base".into(),
            after_fingerprint: "head".into(),
            created_at: Utc::now(),
        };
        preview_reproduction(&manifest, &state, &destination).unwrap();
        reproduce(&manifest, &state, &destination).unwrap();
        assert_eq!(
            std::fs::read_to_string(destination.join("repo/text.txt")).unwrap(),
            "new"
        );
        assert_eq!(
            std::fs::read(destination.join("repo/image.bin")).unwrap(),
            vec![0xff, 0x00]
        );
        assert_eq!(
            preview_reproduction(&manifest, &state, &destination)
                .unwrap_err()
                .error
                .code,
            "machine_reproduction_destination_not_clean"
        );
    }

    /// Copilot must never receive a GitHub URL or a caller's checkout as its
    /// cwd. Its session view is reproduced from the already-cached blobs in a
    /// new app-owned destination. An absolute source root is deliberately
    /// hostile here: accepting it would write outside the session directory.
    #[test]
    fn copilot_github_materialization_uses_cached_blobs_not_remote_or_source_cwd() {
        let temporary = tempfile::tempdir().unwrap();
        let destination = temporary.path().join("copilot-session");
        let state = GithubRoundState {
            round_id: "round-github-copilot".into(),
            payload: GithubQueuePayload {
                locator: GithubPullRequestLocator {
                    host: "github.com".into(),
                    owner: "octo".into(),
                    repository: "private-repo".into(),
                    pull_number: 42,
                },
                metadata: metadata(),
                source_materialized: true,
            },
            files: vec![GithubMaterializedFile {
                path: "src/lib.rs".into(),
                status: "modified".into(),
                base_blob_sha: "base".into(),
                head_blob_sha: "head".into(),
                is_binary: false,
                base_content: Some("before".into()),
                head_content: Some("cached GitHub content".into()),
                base_content_base64: Some("YmVmb3Jl".into()),
                head_content_base64: Some("Y2FjaGVkIEdpdEh1YiBjb250ZW50".into()),
                unified_diff: "-before\n+cached GitHub content".into(),
            }],
            imported_comments: Vec::new(),
            last_staleness: None,
        };
        let source_checkout = "/private/remote-checkouts/private-repo";
        let manifest = crate::WorkspaceManifest {
            workspace_id: "github-pr-42".into(),
            workspace_root: "https://github.com/octo/private-repo/pull/42".into(),
            topic: "PR #42".into(),
            repositories: vec![crate::RepositorySnapshot {
                repository_id: "octo/private-repo".into(),
                root: source_checkout.into(),
                branch: "pull/42".into(),
                base_sha: "base".into(),
                head_sha: "head".into(),
                remote_fingerprint: None,
                object_checksum: "head".into(),
            }],
            before_fingerprint: "base".into(),
            after_fingerprint: "head".into(),
            created_at: Utc::now(),
        };

        let preview = preview_reproduction(&manifest, &state, &destination).unwrap();
        assert!(!preview.writes_original_workspace);
        reproduce(&manifest, &state, &destination).unwrap();

        assert_eq!(
            std::fs::read_to_string(destination.join("octo_private-repo/src/lib.rs")).unwrap(),
            "cached GitHub content"
        );
        assert!(!destination.join("private").exists());
        assert!(!destination.join("remote-checkouts").exists());
        assert!(
            !destination.to_string_lossy().contains(source_checkout),
            "the clean Copilot cwd must not be the original checkout"
        );
    }
}
