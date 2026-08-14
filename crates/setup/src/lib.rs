//! Setup phases report progress for WebSocket delivery to the web UI.

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
