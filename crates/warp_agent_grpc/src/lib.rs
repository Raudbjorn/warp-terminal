//! oh-my-warp: isolated gRPC client + message types for the in-process agent
//! bridge ([`warp`'s `server::server_api::ai::bridge`]).
//!
//! This crate carries its own `prost` 0.13 (via `tonic` 0.12) so the generated
//! proto code never clashes with the warp app crate's `prost` 0.14. `tonic` is
//! re-exported so the bridge uses one matching tonic version (`warp_agent_grpc::tonic`).

pub use tonic;

/// Generated from `proto/agent.proto` (package `agent.v1`) by `build.rs`.
pub mod pb {
    tonic::include_proto!("agent.v1");
}
