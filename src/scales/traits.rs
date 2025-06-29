//! Scale abstraction traits for supporting multiple scale brands
//!
//! This allows the system to work with Bookoo, Acaia, Hario, or other smart scales
//! by implementing a common interface.

use crate::types::ScaleData;
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, channel::Channel};

// Command types that all scales should support
#[derive(Debug, Clone)]
pub enum ScaleCommand {
    Tare,
    StartTimer,
    StopTimer,
    ResetTimer,
}

// Scale capability flags
#[derive(Debug, Clone)]
pub struct ScaleCapabilities {
    pub has_timer: bool,
    pub has_flow_rate: bool,
    pub has_battery_level: bool,
    pub supports_tare: bool,
    pub supports_auto_off: bool,
}

// Scale information
#[derive(Debug, Clone)]
pub struct ScaleInfo {
    pub brand: String,
    pub model: String,
    pub version: Option<String>,
    pub capabilities: ScaleCapabilities,
}

// Status channel for connection state
pub type StatusChannel = Channel<CriticalSectionRawMutex, bool, 2>;
pub type ScaleDataChannel = Channel<CriticalSectionRawMutex, ScaleData, 10>;
pub type ScaleCommandChannel = Channel<CriticalSectionRawMutex, ScaleCommand, 5>;

/// Main trait that all smart scales must implement
/// This trait is object-safe to support dynamic dispatch
pub trait SmartScale: Send + Sync {
    /// Get scale information and capabilities
    fn get_info(&self) -> &ScaleInfo;

    /// Check if scale is currently connected
    fn is_connected(&self) -> bool;
}

/// Helper trait for scales that support BLE
pub trait BleScale: SmartScale {
    /// Get the expected BLE device name pattern
    fn get_ble_name_pattern(&self) -> &str;

    /// Get service UUID for the scale
    fn get_service_uuid(&self) -> uuid::Uuid;

    /// Get data characteristic UUID
    fn get_data_characteristic_uuid(&self) -> uuid::Uuid;

    /// Get command characteristic UUID (if supported)
    fn get_command_characteristic_uuid(&self) -> Option<uuid::Uuid>;

    /// Parse raw BLE data into ScaleData
    fn parse_data(&self, raw_data: &[u8]) -> Result<ScaleData, Box<dyn std::error::Error>>;

    /// Format command for BLE transmission
    fn format_command(&self, command: ScaleCommand) -> Result<Vec<u8>, Box<dyn std::error::Error>>;
}

// Future: trait for WiFi-enabled scales
pub trait WifiScale: SmartScale {
    fn get_ip_address(&self) -> Option<std::net::IpAddr>;
    async fn connect_wifi(
        &mut self,
        ssid: &str,
        password: &str,
    ) -> Result<(), Box<dyn std::error::Error>>;
}

// Future: trait for USB scales
pub trait UsbScale: SmartScale {
    fn get_vendor_id(&self) -> u16;
    fn get_product_id(&self) -> u16;
}
