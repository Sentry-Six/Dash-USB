//! DashUSB setup orchestrator.
//!
//! Each module is one logical setup phase and reports progress through a
//! callback so the web UI can stream live updates over WebSocket.

pub mod apt;
pub mod emitter;
pub mod env;
pub mod error;
pub mod partition;
pub mod disk_images;
pub mod system;
pub mod archive;
pub mod network;
pub mod readonly;
pub mod scripts;
pub mod automount;
pub mod teslacam_mount;
pub mod verify;
pub mod runner;

pub use emitter::SetupEmitter;
pub use error::ConfigError;
