cfg_if::cfg_if! {
    if #[cfg(feature = "completions_v2")] {
        mod completions;
        pub use completions::*;
    }
}
mod commands;
mod events;
mod logging;
mod plugin_host_bootstrap;

pub(super) use commands::*;
pub(super) use events::*;
pub(super) use logging::*;
pub(super) use plugin_host_bootstrap::*;
