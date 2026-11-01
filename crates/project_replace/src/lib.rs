//! Replacing text across the files of a project, and undoing it.
//!
//! The search that finds the matches and the replace that changes them share one configuration
//! ([`engine`]), so that a replace finds what the search showed. A replace only touches a file
//! whose matches are still exactly the ones that were shown, keeps a copy of every file it
//! changes ([`journal`]), and can be undone as a whole ([`undo`]).

pub mod apply;
mod disk;
pub mod encoding;
pub mod engine;
pub mod journal;
pub mod plan;
pub mod preview;
pub mod replacement;
pub mod report;
pub mod scope;
pub mod selection;
#[cfg(test)]
mod test_support;
pub mod undo;
pub mod worker;

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
