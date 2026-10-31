//! WASM stub for the OpenCode sidecar provider.
//!
//! The real [`local_opencode`](super::local_opencode) module spawns a local
//! OpenCode process via `async_process` (a `cfg(not(target_family = "wasm"))`
//! dependency, like `tokio`), which is unavailable on wasm targets. This stub
//! mirrors the public surface so `local_openai` and the rest of the app still
//! compile on wasm; selecting the OpenCode provider there fails with a
//! structured error instead of failing the build.

#![allow(dead_code)]

use std::path::Path;

/// Default OpenCode binary name (settings default; mirrors the real module so
/// settings schemas are identical across targets).
pub const DEFAULT_OPENCODE_BINARY: &str = "opencode";

/// Default OpenCode arguments (settings default; mirrors the real module).
pub const DEFAULT_OPENCODE_ARGS: &[&str] = &["serve", "--port", "0"];

#[derive(Debug, thiserror::Error)]
pub enum OpenCodeError {
    #[error("OpenCode sidecar stdout could not be parsed: {0}")]
    BadAnnouncement(String),
    #[error("OpenCode sidecar is not supported on this platform")]
    Unsupported,
}

/// Stub pool. Cloneable like the real pool, but never spawns a sidecar.
#[derive(Clone, Default)]
pub struct OpenCodeSidecarPool;

impl OpenCodeSidecarPool {
    pub fn new() -> Self {
        Self
    }

    /// No-op: nothing is ever cached on wasm.
    pub fn clear(&self) {}

    /// Always errors on wasm — spawning a local process is unsupported.
    pub async fn get_or_spawn(
        &self,
        _command: &str,
        _args: &[String],
        _working_dir: &Path,
    ) -> Result<OpenCodeSidecar, OpenCodeError> {
        Err(OpenCodeError::Unsupported)
    }
}

/// Stub sidecar. Only ever produced by [`OpenCodeSidecarPool::get_or_spawn`],
/// which always errors on wasm, so the accessors are unreachable at runtime —
/// they exist only to satisfy the shared call sites.
pub struct OpenCodeSidecar {
    base_url: String,
}

impl OpenCodeSidecar {
    pub fn base_url(&self) -> &str {
        &self.base_url
    }
}
