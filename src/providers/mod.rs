//! Data sources. Each module exposes:
//! - `refresh(state, cfg)`: one synchronous update cycle (used by `--dump`),
//! - `spawn(state, cfg, notify)`: a worker thread looping on its interval.
//! Workers never panic the app: errors go into the state's `error` field.

pub mod claude;
pub mod codex;
pub mod sessions;
pub mod status;
pub mod version;
