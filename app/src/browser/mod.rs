//! Embedded browser pane (oh-my-warp).
//!
//! Renders a real browser *inside* Warp's split layout by streaming Chrome's
//! viewport over the Chrome DevTools Protocol (`Page.startScreencast`) and
//! drawing the decoded frames in a GPUI pane. See `BROWSER_PANE_SPEC.md` for the
//! design rationale (Strategy 1: stream frames, don't embed an engine).
//!
//! - [`session`] — spawns Chrome + the CDP screencast thread, decodes frames.
//! - [`view`] — the [`view::BrowserView`] GPUI view that renders frames; wrapped
//!   by [`crate::pane_group::BrowserPane`] to live in the pane tree.

pub mod session;
pub mod view;
