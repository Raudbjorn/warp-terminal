use serde::Serialize;
use serde_json::json;
use strum_macros::{EnumDiscriminants, EnumIter};

use crate::ai::agent_management::cloud_setup_guide_view::SetupGuideDocs;

/// Which setup guide workflow step the user interacted with
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SetupGuideStep {
    /// Quick start banner: Visit Oz
    VisitOz,
    /// Step 1: Create environment (slash command)
    CreateEnvironment,
    /// Step 1: Create environment (CLI command)
    CreateEnvironmentCli,
    /// Step 2: Create Slack integration
    CreateSlackIntegration,
    /// Step 2: Create Linear integration
    CreateLinearIntegration,
}

/// Where the item was opened from
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OpenedFrom {
    ManagementView,
    ConversationList,
    DetailsPanel,
}

/// Type of artifact clicked
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactType {
    Plan,
    Branch,
    PullRequest,
    File,
}

/// Type of filter changed
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FilterType {
    Status,
    Source,
    CreatedOn,
    Creator,
    Owner,
    Harness,
}
