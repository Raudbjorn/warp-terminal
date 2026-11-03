use serde_json::{Value, json};
use strum_macros::{EnumDiscriminants, EnumIter};
use warp_core::features::FeatureFlag;

use crate::workspace::tab_settings::{
    VerticalTabsCompactSubtitle, VerticalTabsDisplayGranularity, VerticalTabsPrimaryInfo,
    VerticalTabsTabItemMode, VerticalTabsViewMode,
};

/// Which display option on the vertical tabs settings popup the user changed,
/// along with the new value they picked.
#[derive(Clone, Copy, Debug)]
pub enum VerticalTabsDisplayOption {
    DisplayGranularity(VerticalTabsDisplayGranularity),
    TabItemMode(VerticalTabsTabItemMode),
    ViewMode(VerticalTabsViewMode),
    PrimaryInfo(VerticalTabsPrimaryInfo),
    CompactSubtitle(VerticalTabsCompactSubtitle),
    ShowPrLink(bool),
    ShowDiffStats(bool),
    ShowDetailsOnHover(bool),
}

impl VerticalTabsDisplayOption {
    fn option_name(&self) -> &'static str {
        match self {
            Self::DisplayGranularity(_) => "display_granularity",
            Self::TabItemMode(_) => "tab_item_mode",
            Self::ViewMode(_) => "view_mode",
            Self::PrimaryInfo(_) => "primary_info",
            Self::CompactSubtitle(_) => "compact_subtitle",
            Self::ShowPrLink(_) => "show_pr_link",
            Self::ShowDiffStats(_) => "show_diff_stats",
            Self::ShowDetailsOnHover(_) => "show_details_on_hover",
        }
    }

    fn serialized_value(&self) -> Value {
        match self {
            Self::DisplayGranularity(VerticalTabsDisplayGranularity::Panes) => json!("panes"),
            Self::DisplayGranularity(VerticalTabsDisplayGranularity::Tabs) => json!("tabs"),
            Self::TabItemMode(VerticalTabsTabItemMode::FocusedSession) => json!("focused_session"),
            Self::TabItemMode(VerticalTabsTabItemMode::Summary) => json!("summary"),
            Self::ViewMode(VerticalTabsViewMode::Compact) => json!("compact"),
            Self::ViewMode(VerticalTabsViewMode::Expanded) => json!("expanded"),
            Self::PrimaryInfo(VerticalTabsPrimaryInfo::Command) => json!("command"),
            Self::PrimaryInfo(VerticalTabsPrimaryInfo::WorkingDirectory) => {
                json!("working_directory")
            }
            Self::PrimaryInfo(VerticalTabsPrimaryInfo::Branch) => json!("branch"),
            Self::CompactSubtitle(VerticalTabsCompactSubtitle::Branch) => json!("branch"),
            Self::CompactSubtitle(VerticalTabsCompactSubtitle::WorkingDirectory) => {
                json!("working_directory")
            }
            Self::CompactSubtitle(VerticalTabsCompactSubtitle::Command) => json!("command"),
            Self::ShowPrLink(value) => json!(value),
            Self::ShowDiffStats(value) => json!(value),
            Self::ShowDetailsOnHover(value) => json!(value),
        }
    }
}

/// Where in the vertical tabs UI a clickable diff-stats or GitHub PR chip
/// was rendered when the user clicked it.
#[derive(Clone, Copy, Debug)]
pub enum VerticalTabsChipEntrypoint {
    /// The chip was rendered on a row representing a single pane
    /// (display granularity: Panes).
    Pane,
    /// The chip was rendered on a row representing a tab group
    /// (display granularity: Tabs).
    Tab,
    /// The chip was rendered inside the detail sidecar that appears on row hover.
    DetailsSidecar,
}

impl VerticalTabsChipEntrypoint {
    fn serialized(&self) -> &'static str {
        match self {
            Self::Pane => "pane",
            Self::Tab => "tab",
            Self::DetailsSidecar => "details_sidecar",
        }
    }
}
