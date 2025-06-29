// Core modules
pub mod ble;
pub mod brewing;
pub mod hardware;
pub mod scales;
pub mod server;
pub mod system;
pub mod wifi;

// Legacy modules (to be refactored)
pub mod controller;
pub mod state;
pub mod types;

pub use controller::*;
pub use types::*;
