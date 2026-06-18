//! oh-my-warp: command-palette data source for commands registered by JS plugins.
//!
//! Reads the live [`crate::plugin::commands`] registry and yields a synthetic [`CommandBinding`]
//! per plugin command (named `plugin:<id>`), reusing the existing binding rendering. The palette's
//! accept handler recognizes the `plugin:` prefix and invokes the plugin callback. See
//! `command_palette/view.rs` and PLUGIN_SPEC.md (Milestone 1).

use std::sync::Arc;

use fuzzy_match::{match_indices_case_insensitive, FuzzyMatchResult};
use warpui::{AppContext, Entity};

use crate::search::action::search_item::MatchedBinding;
use crate::search::command_palette::mixer::CommandPaletteItemAction;
use crate::search::data_source::{Query, QueryResult};
use crate::search::mixer::{DataSourceRunErrorWrapper, SyncDataSource};
use crate::util::bindings::CommandBinding;

/// Prefix on the synthetic [`CommandBinding`] name so the palette accept handler can recognize a
/// plugin command and route it to the plugin host instead of dispatching a typed action.
pub const PLUGIN_COMMAND_BINDING_PREFIX: &str = "plugin:";

/// Data source that surfaces plugin-registered commands in the command palette.
pub struct PluginCommandDataSource;

impl Entity for PluginCommandDataSource {
    type Event = ();
}

impl SyncDataSource for PluginCommandDataSource {
    type Action = CommandPaletteItemAction;

    fn run_query(
        &self,
        query: &Query,
        _app: &AppContext,
    ) -> Result<Vec<QueryResult<Self::Action>>, DataSourceRunErrorWrapper> {
        let term = query.text.trim().to_lowercase();
        Ok(crate::plugin::commands::all()
            .into_iter()
            .filter_map(|command| {
                let match_result = if term.is_empty() {
                    FuzzyMatchResult::no_match()
                } else {
                    match_indices_case_insensitive(&command.title.to_lowercase(), &term)?
                };
                let binding = CommandBinding::new(
                    format!("{PLUGIN_COMMAND_BINDING_PREFIX}{}", command.id),
                    command.title,
                    None,
                );
                Some(MatchedBinding::new(match_result, Arc::new(binding)).into())
            })
            .collect())
    }
}
