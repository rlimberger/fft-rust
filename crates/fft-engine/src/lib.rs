//! Dedicated-thread market engine, frozen command protocol, and coherent
//! latest-value render snapshots.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod command;
mod service;
mod snapshot;

pub use command::{EngineCmd, LiveConfig, Source};
pub use service::{
    EngineConfig, EngineExit, EngineHandle, EngineService, EngineStateError, Watermarks,
};
pub use snapshot::{
    CoverageCounters, DomPriceRow, DomRenderState, PriceRefreshRender, ProfilePriceRow,
    ProfileRenderState, ProfileSessionRender, RenderSnapshot, SnapshotSlot, build_snapshot,
};
