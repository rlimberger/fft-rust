//! Dedicated-thread market engine, frozen command protocol, and coherent
//! latest-value render snapshots.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod checkpoint;
mod command;
mod forward;
mod live_log;
mod pacing;
mod prior;
mod runtime;
mod service;
mod sim_live;
mod snapshot;
mod watermarks;

pub use checkpoint::{
    CHECKPOINT_EVENT_CADENCE_NS, CheckpointError, CheckpointSummary, write_checkpointed_copy,
};
pub use command::{EngineCmd, LiveConfig, Source};
pub use pacing::APPLY_BUDGET;
pub use service::{EngineConfig, EngineExit, EngineHandle, EngineService, EngineStateError};
pub use snapshot::{
    CoverageCounters, DomPriceRow, DomRenderState, PriceRefreshRender, ProfilePriceRow,
    ProfileRenderState, ProfileSessionRender, RenderSnapshot, SnapshotSlot, build_snapshot,
};
pub use watermarks::Watermarks;
