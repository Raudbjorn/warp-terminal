use project_replace::scope::ScopeGlobs;
use warp_ripgrep::search::Submatch;
use warp_util::local_or_remote_path::LocalOrRemotePath;

pub struct SearchConfig {
    pub use_regex: bool,
    pub use_case_sensitivity: bool,
}

/// What narrows a global search beyond its query.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SearchOptions {
    /// Whether text matches have to be whole words. Names are matched as before.
    pub whole_word: bool,
    /// The files and folders the search covers.
    pub scope: ScopeGlobs,
}

/// A single global search match: one line in one file, which may live on
/// the local filesystem or on a remote host.
#[derive(Clone, Debug)]
pub struct GlobalSearchMatch {
    pub location: LocalOrRemotePath,
    pub line_number: u32,
    /// Original 1-based character column in the file. This is captured
    /// before display-only whitespace trimming so opening a result navigates
    /// to the correct location.
    pub column_num: Option<usize>,
    pub line_text: String,
    pub submatches: Vec<Submatch>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NameMatchKind {
    Folder,
    File,
}

/// A file or folder whose name matched the query.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NameMatch {
    pub location: LocalOrRemotePath,
    /// Path shown for the match, relative to the search root it was found under.
    pub display_path: String,
    /// Char indices into `display_path` of the characters the query matched.
    pub highlight_indices: Vec<usize>,
    pub score: i64,
}

/// The best-scoring name matches of one kind, best first.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RankedNameMatches {
    pub matches: Vec<NameMatch>,
    /// Number of entries that matched, which exceeds `matches.len()` when the weakest ones were
    /// dropped.
    pub total: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NameSearchResults {
    pub folders: RankedNameMatches,
    pub files: RankedNameMatches,
}

#[cfg_attr(not(target_family = "wasm"), path = "model.rs")]
#[cfg_attr(target_family = "wasm", path = "model_wasm.rs")]
pub mod model;
mod name_groups;
#[cfg(not(target_family = "wasm"))]
mod name_match;
#[cfg(not(target_family = "wasm"))]
mod name_search;
#[cfg_attr(feature = "local_fs", path = "replace_model.rs")]
#[cfg_attr(not(feature = "local_fs"), path = "replace_model_stub.rs")]
pub mod replace_model;
pub mod view;
