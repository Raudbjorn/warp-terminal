use std::fmt::Display;
use std::time::Duration;

use serde::{Serialize, Serializer};
use serde_json::json;
use serde_with::SerializeDisplay;
use strum_macros::{EnumDiscriminants, EnumIter};

use crate::code_review::diff_state::{BackendOrigin, DiffMode, DiffOperation};
use crate::features::FeatureFlag;
use crate::terminal::CLIAgent;
use crate::view_components::find::FindDirection;

/// Identifies which git button the user clicked in the code review header.
/// Each variant maps to one of the primary action button / dropdown items.
#[derive(Clone, Copy, Debug, Serialize)]
pub enum GitButtonKind {
    #[serde(rename = "commit")]
    Commit,
    #[serde(rename = "push")]
    Push,
    #[serde(rename = "publish")]
    Publish,
    #[serde(rename = "create_pr")]
    CreatePr,
    #[serde(rename = "view_pr")]
    ViewPr,
}

/// Identifies which git operation actually ran when a `GitDialog` completed.
/// Distinguishes commit-dialog chained intents (e.g. commit-and-push) from
/// standalone push/publish/create-PR dialogs so analytics can tell the user
/// flows apart.
#[derive(Clone, Copy, Debug, Serialize)]
pub enum GitOperationKind {
    /// Commit dialog with the commit-only intent.
    #[serde(rename = "commit_only")]
    CommitOnly,
    /// Commit dialog with the commit-and-push intent.
    #[serde(rename = "commit_and_push")]
    CommitAndPush,
    /// Commit dialog with the commit-and-create-PR intent.
    #[serde(rename = "commit_and_create_pr")]
    CommitAndCreatePr,
    /// Standalone push dialog.
    #[serde(rename = "push")]
    Push,
    /// Standalone publish dialog (push that also sets upstream).
    #[serde(rename = "publish")]
    Publish,
    /// Standalone create-PR dialog.
    #[serde(rename = "create_pr")]
    CreatePr,
}

/// Terminal status of a `GitDialog`. Captures both async-op outcomes and
/// pre-confirmation user cancels in a single enum.
#[derive(Clone, Copy, Debug, Serialize)]
pub enum GitDialogStatus {
    /// User confirmed the dialog and the underlying git operation succeeded.
    #[serde(rename = "succeeded")]
    Succeeded,
    /// User confirmed the dialog and the underlying git operation failed.
    #[serde(rename = "failed")]
    Failed,
    /// User cancelled the dialog (ESC / close button / cancel button) before
    /// the async op ran.
    #[serde(rename = "cancelled")]
    Cancelled,
}

/// Entry points for opening the code review pane.
#[derive(Clone, Copy, Debug, SerializeDisplay, Default)]
pub enum CodeReviewPaneEntrypoint {
    /// Opened via the git diff chip (git changes button in AI control panel).
    GitDiffChip,
    /// Opened via the "View changes" button when Agent mode is done running.
    AgentModeCompleted,
    /// Opened via the "Review changes" button when Agent mode is running.
    AgentModeRunning,
    /// Opened via the "/code-review" slash command.
    SlashCommand,
    /// Opened by the agent tool call.
    InvokedByAgent,
    // Force opened when user accepted first diff of a conversation
    ForceOpened,
    // Opened via the agent mode diff header
    CodeDiffHeader,
    // Opened via the pane header
    PaneHeader,
    // Opened via the code mode v2 right panel button
    RightPanel,
    /// Opened via the CLI agent view footer (e.g., Claude Code).
    CLIAgentView,
    /// Opened via other means (unknown entry point).
    #[default]
    Other,
}

impl Display for CodeReviewPaneEntrypoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::GitDiffChip => write!(f, "git_diff_chip"),
            Self::AgentModeCompleted => write!(f, "agent_mode_completed"),
            Self::AgentModeRunning => write!(f, "agent_mode_running"),
            Self::SlashCommand => write!(f, "slash_command"),
            Self::InvokedByAgent => write!(f, "invoked_by_agent"),
            Self::ForceOpened => write!(f, "force_opened"),
            Self::CodeDiffHeader => write!(f, "agent_mode_diff_header"),
            Self::PaneHeader => write!(f, "pane_header"),
            Self::RightPanel => write!(f, "right_panel"),
            Self::CLIAgentView => write!(f, "cli_agent_view"),
            Self::Other => write!(f, "other"),
        }
    }
}

/// Origin of an "Add to context" action.
#[derive(Clone, Copy, Debug, Serialize)]
pub enum AddToContextOrigin {
    /// User selected text and added it to context.
    #[serde(rename = "selected_text")]
    SelectedText,
    /// User clicked the gutter to add a line/hunk to context.
    #[serde(rename = "gutter")]
    Gutter,
    /// User clicked the "Add diff set as context" button in code review header.
    #[serde(rename = "code_review_header")]
    #[allow(unused)]
    CodeReviewHeader,
}

/// Where code review content was sent after the user action.
#[derive(Clone, Copy, Debug, Serialize)]
pub enum CodeReviewContextDestination {
    /// Written directly to the terminal PTY for an active CLI agent.
    #[serde(rename = "pty")]
    Pty,
    /// Inserted into the Warp AI input buffer as plain text.
    #[serde(rename = "agent_input")]
    AgentInput,
    /// Registered as an AI attachment and referenced from the input.
    #[serde(rename = "agent_attachment")]
    AgentAttachment,
    /// Inserted into the active command buffer while a command is running.
    #[serde(rename = "active_command_buffer")]
    ActiveCommandBuffer,
    /// Submitted as an inline code review request through the Warp AI path.
    #[serde(rename = "agent_review")]
    AgentReview,
    /// Inserted into CLI agent rich input.
    #[serde(rename = "rich_input")]
    RichInput,
}

/// Scope of a diff set attachment initiated from code review.
#[derive(Clone, Copy, Debug, Serialize)]
#[cfg_attr(not(feature = "local_fs"), allow(dead_code))]
pub enum DiffSetContextScope {
    /// Attach the full diff set for the current review.
    #[serde(rename = "all")]
    All,
    /// Attach the diff set for a single file.
    #[serde(rename = "file")]
    File,
}

/// Pane state change for minimize/maximize events.
#[derive(Clone, Copy, Debug, Serialize)]
pub enum PaneStateChange {
    /// Pane was minimized.
    #[serde(rename = "minimized")]
    Minimized,
    /// Pane was maximized.
    #[serde(rename = "maximized")]
    Maximized,
}

fn serialize_optional_cli_agent_telemetry_name<S: Serializer>(
    agent: &Option<CLIAgent>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    agent
        .map(|agent| agent.telemetry_name())
        .serialize(serializer)
}
