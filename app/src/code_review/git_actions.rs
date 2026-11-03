//! Shared git "action" orchestration: the commit-chain, push, create-PR, and
//! view-PR workflows behind the code-review git buttons.
//!
//! These compose the single-command primitives in [`crate::util::git`] (plus
//! AI title/body generation) into the end-to-end actions a button triggers.
//! They are intentionally backend-agnostic: the local code-review dialog and
//! the remote-server daemon both call them, so local and remote behave
//! identically. Git ops are host-scoped and not tied to a diff-state model, so
//! this logic lives here rather than on a model.
//!
//! Callers own everything *around* the action: UI (toasts, telemetry, dialog
//! lifecycle), transport/model (applying the returned delta to a
//! `DiffStateModel`, building wire responses), and any execution-time guards
//! (e.g. the daemon's `git_operation_in_progress` backstop).

use std::path::Path;

use crate::ai::generate_code_review_content::api::{GenerateCodeReviewContentRequest, OutputType};
use crate::code_review::diff_state::CommitChainMode;
use crate::server::server_api::ai::AIClient;
#[cfg(feature = "local_fs")]
use crate::util::git::TrackingCounts;
use crate::util::git::{self, Commit, PrInfo, get_branch_commit_messages, get_diff_for_pr};

#[cfg(all(test, feature = "local_fs"))]
#[path = "git_actions_tests.rs"]
mod tests;

/// What bringing a branch level with its upstream takes, decided after a fetch.
#[cfg(feature = "local_fs")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncPlan {
    /// Another git operation is under way in the working tree.
    Blocked,
    NoUpstream,
    UpToDate,
    /// Both sides have commits the other lacks. Reconciling them takes a merge or a rebase,
    /// which can stop on conflicts, so it is left to the user.
    Diverged {
        ahead: usize,
        behind: usize,
    },
    /// Fast-forward to the upstream's commits.
    Pull {
        behind: usize,
    },
    /// Send the local commits upstream.
    Push {
        ahead: usize,
    },
}

/// Chooses how to sync a branch whose remote was just fetched.
#[cfg(feature = "local_fs")]
pub fn sync_plan(tracking: Option<TrackingCounts>, operation_in_progress: bool) -> SyncPlan {
    if operation_in_progress {
        return SyncPlan::Blocked;
    }
    match tracking {
        None => SyncPlan::NoUpstream,
        Some(TrackingCounts {
            ahead: 0,
            behind: 0,
        }) => SyncPlan::UpToDate,
        Some(TrackingCounts { ahead, behind: 0 }) => SyncPlan::Push { ahead },
        Some(TrackingCounts { ahead: 0, behind }) => SyncPlan::Pull { behind },
        Some(TrackingCounts { ahead, behind }) => SyncPlan::Diverged { ahead, behind },
    }
}

/// Why a sync stopped without changing anything.
#[cfg(feature = "local_fs")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncRefusal {
    OperationInProgress,
    NoUpstream,
    Diverged { ahead: usize, behind: usize },
}

/// Result of a sync that ran to the end.
#[cfg(feature = "local_fs")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncOutcome {
    Refused(SyncRefusal),
    Completed {
        /// Upstream commits the branch was fast-forwarded over.
        pulled: usize,
        /// Local commits sent upstream.
        pushed: usize,
    },
}

/// Fetches, then fast-forwards `branch` to its upstream or pushes its new commits there.
///
/// Never merges or rebases: a branch that has diverged from its upstream is left untouched and
/// reported as refused.
#[cfg(feature = "local_fs")]
pub async fn run_sync(
    repo_path: &Path,
    branch: &str,
    path_env: Option<&str>,
) -> anyhow::Result<SyncOutcome> {
    if git::detect_current_branch(repo_path).await? != branch {
        anyhow::bail!("The checked-out branch changed; refresh source control before syncing.");
    }
    if git::git_operation_in_progress(repo_path) {
        return Ok(SyncOutcome::Refused(SyncRefusal::OperationInProgress));
    }
    if git::push_destination(repo_path, branch).await.is_none()
        || git::tracking_counts(repo_path).await?.is_none()
    {
        return Ok(SyncOutcome::Refused(SyncRefusal::NoUpstream));
    }
    git::run_fetch(repo_path, path_env).await?;
    if git::detect_current_branch(repo_path).await? != branch {
        anyhow::bail!(
            "The checked-out branch changed during fetch; refresh source control before syncing."
        );
    }

    let tracking = git::tracking_counts(repo_path).await?;
    let plan = sync_plan(tracking, git::git_operation_in_progress(repo_path));
    match plan {
        SyncPlan::Blocked => Ok(SyncOutcome::Refused(SyncRefusal::OperationInProgress)),
        SyncPlan::NoUpstream => Ok(SyncOutcome::Refused(SyncRefusal::NoUpstream)),
        SyncPlan::Diverged { ahead, behind } => Ok(SyncOutcome::Refused(SyncRefusal::Diverged {
            ahead,
            behind,
        })),
        SyncPlan::UpToDate => Ok(SyncOutcome::Completed {
            pulled: 0,
            pushed: 0,
        }),
        SyncPlan::Pull { behind } => {
            git::run_pull_ff_only(repo_path, path_env).await?;
            Ok(SyncOutcome::Completed {
                pulled: behind,
                pushed: 0,
            })
        }
        SyncPlan::Push { ahead } => {
            git::run_push(repo_path, branch, path_env).await?;
            Ok(SyncOutcome::Completed {
                pulled: 0,
                pushed: ahead,
            })
        }
    }
}

/// Runs the commit chain — always commits, then optionally pushes, then
/// optionally creates a PR per `mode` — and returns the post-chain delta
/// (refreshed unpushed commits + upstream ref) plus any created PR. The delta
/// is computed once after the whole chain settles.
///
/// When the chain creates a PR, `ai_client` (when `Some`) generates the
/// title/body with a `--fill` fallback; pass `None` to skip AI entirely.
pub async fn run_commit_chain(
    repo_path: &Path,
    mode: CommitChainMode,
    message: &str,
    include_unstaged: bool,
    branch: &str,
    ai_client: Option<&dyn AIClient>,
    path_env: Option<&str>,
) -> anyhow::Result<(Vec<Commit>, Option<String>, Option<PrInfo>)> {
    git::run_commit(repo_path, message, include_unstaged, path_env).await?;
    let pr_info = match mode {
        CommitChainMode::CommitOnly => None,
        CommitChainMode::CommitAndPush => {
            git::run_push(repo_path, branch, path_env).await?;
            None
        }
        CommitChainMode::CommitAndCreatePr => {
            git::run_push(repo_path, branch, path_env).await?;
            Some(create_pr(repo_path, branch, ai_client, path_env).await?)
        }
    };
    let (commits, upstream_ref) = git::compute_unpushed_state(repo_path).await;
    Ok((commits, upstream_ref, pr_info))
}

/// Pushes `branch` (setting upstream) and returns the refreshed
/// unpushed/upstream delta.
pub async fn run_push(
    repo_path: &Path,
    branch: &str,
    path_env: Option<&str>,
) -> anyhow::Result<(Vec<Commit>, Option<String>)> {
    git::run_push(repo_path, branch, path_env).await?;
    Ok(git::compute_unpushed_state(repo_path).await)
}

/// Creates a PR for `branch`. When `ai_client` is `Some`, generates the
/// title/body via AI with a `gh pr create --fill` fallback; otherwise creates
/// the PR with `--fill`.
pub async fn create_pr(
    repo_path: &Path,
    branch: &str,
    ai_client: Option<&dyn AIClient>,
    path_env: Option<&str>,
) -> anyhow::Result<PrInfo> {
    match ai_client {
        Some(ai) => create_pr_with_ai_content(repo_path, branch, ai, path_env).await,
        None => git::create_pr(repo_path, None, None, path_env).await,
    }
}

/// Generates an AI commit message for the working-tree changes.
/// Bails when there's nothing to summarize (empty diff) or the model returns an empty message.
pub async fn generate_commit_message(
    repo_path: &Path,
    branch_name: &str,
    include_unstaged: bool,
    ai_client: &dyn AIClient,
) -> anyhow::Result<String> {
    let diff = git::get_diff_for_commit_message(repo_path, include_unstaged).await?;
    // Skip the AI round trip when there's nothing to summarize.
    if diff.trim().is_empty() {
        anyhow::bail!("no changes to generate a commit message from");
    }
    let generated = ai_client
        .generate_code_review_content(GenerateCodeReviewContentRequest {
            output_type: OutputType::CommitMessage,
            diff,
            branch_name: branch_name.to_string(),
            commit_messages: Vec::new(),
        })
        .await?
        .content;
    let trimmed = generated.trim();
    if trimmed.is_empty() {
        anyhow::bail!("AI returned an empty commit message");
    }
    Ok(trimmed.to_string())
}

/// Generates PR title and body via AI (in parallel) and creates the PR.
/// Falls back to `gh pr create --fill` if AI generation fails or returns
/// empty content, so AI-assisted and manual PR creation produce PRs the same
/// way.
async fn create_pr_with_ai_content(
    repo_path: &Path,
    branch_name: &str,
    code_review_ai: &dyn AIClient,
    path_env: Option<&str>,
) -> anyhow::Result<PrInfo> {
    let diff = get_diff_for_pr(repo_path).await?;
    let commit_messages = get_branch_commit_messages(repo_path)
        .await
        .unwrap_or_default();

    let title_req = GenerateCodeReviewContentRequest {
        output_type: OutputType::PrTitle,
        diff: diff.clone(),
        branch_name: branch_name.to_string(),
        commit_messages: commit_messages.clone(),
    };
    let body_req = GenerateCodeReviewContentRequest {
        output_type: OutputType::PrDescription,
        diff,
        branch_name: branch_name.to_string(),
        commit_messages,
    };

    match futures::try_join!(
        code_review_ai.generate_code_review_content(title_req),
        code_review_ai.generate_code_review_content(body_req),
    ) {
        Ok((title_resp, body_resp))
            if !title_resp.content.trim().is_empty() && !body_resp.content.trim().is_empty() =>
        {
            git::create_pr(
                repo_path,
                Some(&title_resp.content),
                Some(&body_resp.content),
                path_env,
            )
            .await
        }
        Ok(_) => {
            // Empty title/body would make `gh pr create` fail; fall back to --fill.
            log::warn!(
                "AI PR content generation returned empty title/body, falling back to --fill"
            );
            git::create_pr(repo_path, None, None, path_env).await
        }
        Err(err) => {
            log::warn!("AI PR content generation failed, falling back to --fill: {err}");
            git::create_pr(repo_path, None, None, path_env).await
        }
    }
}
