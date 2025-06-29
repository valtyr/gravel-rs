//! Simplified Scale Scanner Implementation
//! 
//! This is a practical implementation of the generic scanner that focuses
//! on the Bookoo scale while providing the architecture for future expansion.

use crate::{
    ble::{BleClient, Device, DeviceFilter, StatusChannel},
    scales::{
        bookoo::BookooScale,
        traits::{ScaleDataChannel, SmartScale},
    },
};
use embassy_time::{Duration, Timer};
use log::{debug, error, info, warn};
use std::sync::Arc;

/// Error types for scale scanning operations
#[derive(Debug)]
pub enum ScanError {
    BleError(String),
    NoDevicesFound,
    NoCompatibleScales,
    ConnectionFailed(String),
    Timeout,
}

impl std::fmt::Display for ScanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ScanError::BleError(msg) => write!(f, "BLE error: {}", msg),
            ScanError::NoDevicesFound => write!(f, "No BLE devices found during scan"),
            ScanError::NoCompatibleScales => write!(f, "No compatible scales detected"),
            ScanError::ConnectionFailed(msg) => write!(f, "Connection failed: {}", msg),
            ScanError::Timeout => write!(f, "Scan timeout"),
        }
    }
}

impl std::error::Error for ScanError {}

/// Scale types that can be detected
#[derive(Debug, Clone, PartialEq)]
pub enum ScaleType {
    Bookoo,
    // Future: Acaia, Hario, etc.
}

impl ScaleType {
    pub fn as_str(&self) -> &'static str {
        match self {
            ScaleType::Bookoo => "Bookoo",
        }
    }
}

/// Information about a detected scale candidate
#[derive(Debug, Clone)]
pub struct ScaleCandidate {
    pub device: Device,
    pub scale_type: ScaleType,
    pub confidence: u8, // 0-100, how confident we are this is the right scale type
}

/// Simple scale scanner that can detect and connect to Bookoo scales
/// 
/// This implementation focuses on practical functionality while providing
/// a foundation for future multi-scale support.
pub struct SimpleScaleScanner {
    ble_client: BleClient,
    data_channel: Arc<ScaleDataChannel>,
    status_channel: Arc<StatusChannel>,
    scan_timeout_ms: u32,
}

impl SimpleScaleScanner {
    /// Create a new scale scanner
    pub fn new(
        data_channel: Arc<ScaleDataChannel>,
        status_channel: Arc<StatusChannel>,
    ) -> Self {
        Self {
            ble_client: BleClient::new(Arc::clone(&status_channel)),
            data_channel,
            status_channel,
            scan_timeout_ms: 10000, // 10 seconds default
        }
    }

    /// Set scan timeout in milliseconds
    pub fn set_scan_timeout(&mut self, timeout_ms: u32) {
        self.scan_timeout_ms = timeout_ms;
    }

    /// Scan for any compatible scale and connect to the best one found
    pub async fn scan_and_connect(&mut self) -> Result<Box<BookooScale>, ScanError> {
        info!("🔍 Starting scale scan...");
        
        // For now, we only support Bookoo scales
        match self.scan_for_bookoo_scale().await {
            Ok(scale) => {
                info!("✅ Successfully connected to Bookoo scale");
                Ok(Box::new(scale))
            }
            Err(e) => {
                error!("❌ Failed to find/connect to Bookoo scale: {:?}", e);
                Err(e)
            }
        }
    }

    /// Scan for a specific scale type
    pub async fn scan_for_scale_type(&mut self, scale_type: ScaleType) -> Result<Box<BookooScale>, ScanError> {
        match scale_type {
            ScaleType::Bookoo => {
                info!("🎯 Scanning specifically for Bookoo scales");
                self.scan_for_bookoo_scale().await.map(|s| Box::new(s))
            }
        }
    }

    /// Get list of supported scale types
    pub fn get_supported_scale_types(&self) -> Vec<ScaleType> {
        vec![ScaleType::Bookoo]
    }

    /// Scan for Bookoo scales specifically
    async fn scan_for_bookoo_scale(&mut self) -> Result<BookooScale, ScanError> {
        info!("🔍 Scanning for Bookoo scales...");

        // Discover all BLE devices
        let devices = self.discover_ble_devices().await?;
        if devices.is_empty() {
            return Err(ScanError::NoDevicesFound);
        }

        // Look for Bookoo devices
        let bookoo_candidates = self.find_bookoo_candidates(devices);
        if bookoo_candidates.is_empty() {
            warn!("❌ No Bookoo scales found among discovered devices");
            return Err(ScanError::NoCompatibleScales);
        }

        // Try to connect to the best candidate
        self.connect_to_bookoo_candidate(bookoo_candidates).await
    }

    /// Discover all nearby BLE devices
    async fn discover_ble_devices(&mut self) -> Result<Vec<Device>, ScanError> {
        debug!("🔍 Starting BLE device discovery ({}ms timeout)", self.scan_timeout_ms);
        
        let filter = DeviceFilter {
            name_prefix: None, // Discover all devices
            service_uuid: None,
        };
        
        match self.ble_client.scan_for_devices(Some(filter), self.scan_timeout_ms).await {
            Ok(devices) => {
                debug!("📡 Discovered {} BLE devices", devices.len());
                for device in &devices {
                    debug!("  - Device: {:?}", device.name);
                }
                Ok(devices)
            }
            Err(e) => {
                error!("🚨 BLE device discovery failed: {:?}", e);
                Err(ScanError::BleError(format!("Discovery failed: {:?}", e)))
            }
        }
    }

    /// Find Bookoo scale candidates among discovered devices
    fn find_bookoo_candidates(&self, devices: Vec<Device>) -> Vec<ScaleCandidate> {
        let mut candidates = Vec::new();

        for device in devices {
            if let Some(ref name) = device.name {
                // Check for Bookoo device patterns
                if name.starts_with("BOOKOO_SC") {
                    candidates.push(ScaleCandidate {
                        device,
                        scale_type: ScaleType::Bookoo,
                        confidence: 100, // Very confident - exact prefix match
                    });
                } else if name.contains("BOOKOO") {
                    candidates.push(ScaleCandidate {
                        device,
                        scale_type: ScaleType::Bookoo,
                        confidence: 90, // High confidence - contains brand name
                    });
                }
            }
        }

        // Sort by confidence (highest first)
        candidates.sort_by_key(|c| std::cmp::Reverse(c.confidence));

        if !candidates.is_empty() {
            info!("✅ Found {} Bookoo scale candidate(s)", candidates.len());
            for candidate in &candidates {
                debug!("  - {} (confidence: {}%)", 
                       candidate.device.name.as_deref().unwrap_or("Unknown"),
                       candidate.confidence);
            }
        }

        candidates
    }

    /// Connect to the best Bookoo scale candidate
    async fn connect_to_bookoo_candidate(&self, candidates: Vec<ScaleCandidate>) -> Result<BookooScale, ScanError> {
        for candidate in candidates {
            info!("🔗 Attempting to connect to Bookoo scale: {:?}", candidate.device.name);
            
            // Create a new BookooScale instance
            let mut bookoo_scale = BookooScale::new(
                Arc::clone(&self.data_channel),
                Arc::clone(&self.status_channel),
            );

            // Try to connect to this specific device
            match bookoo_scale.connect_to_device(candidate.device.clone()).await {
                Ok(()) => {
                    info!("🎉 Successfully connected to Bookoo scale!");
                    return Ok(bookoo_scale);
                }
                Err(e) => {
                    warn!("❌ Failed to connect to candidate: {:?} - trying next...", e);
                    
                    // Brief delay before trying next candidate
                    Timer::after(Duration::from_millis(500)).await;
                    continue;
                }
            }
        }

        error!("💀 All Bookoo connection attempts failed");
        Err(ScanError::ConnectionFailed("All candidates failed".to_string()))
    }

    /// Scan and connect with retry logic
    pub async fn scan_and_connect_with_retry(&mut self, max_attempts: u32, retry_delay_ms: u64) -> Result<Box<BookooScale>, ScanError> {
        for attempt in 1..=max_attempts {
            info!("🔄 Scale scan attempt {}/{}", attempt, max_attempts);
            
            match self.scan_and_connect().await {
                Ok(scale) => {
                    info!("✅ Scale connection successful on attempt {}", attempt);
                    return Ok(scale);
                }
                Err(e) => {
                    if attempt < max_attempts {
                        warn!("❌ Scan attempt {} failed: {:?} - retrying in {}ms", 
                              attempt, e, retry_delay_ms);
                        Timer::after(Duration::from_millis(retry_delay_ms)).await;
                    } else {
                        error!("💀 All {} scan attempts failed: {:?}", max_attempts, e);
                        return Err(e);
                    }
                }
            }
        }
        
        Err(ScanError::Timeout)
    }
}

/// Configuration for scale scanning behavior
#[derive(Debug, Clone)]
pub struct ScanConfig {
    pub scan_timeout_ms: u32,
    pub max_retry_attempts: u32,
    pub retry_delay_ms: u64,
    pub preferred_scale_types: Vec<ScaleType>,
}

impl Default for ScanConfig {
    fn default() -> Self {
        Self {
            scan_timeout_ms: 10000,
            max_retry_attempts: 3,
            retry_delay_ms: 2000,
            preferred_scale_types: vec![ScaleType::Bookoo],
        }
    }
}

/// Scale management helper that handles connection lifecycle
pub struct ScaleManager {
    scanner: SimpleScaleScanner,
    current_scale: Option<Box<BookooScale>>,
    config: ScanConfig,
}

impl ScaleManager {
    /// Create a new scale manager
    pub fn new(
        data_channel: Arc<ScaleDataChannel>,
        status_channel: Arc<StatusChannel>,
        config: ScanConfig,
    ) -> Self {
        let mut scanner = SimpleScaleScanner::new(data_channel, status_channel);
        scanner.set_scan_timeout(config.scan_timeout_ms);

        Self {
            scanner,
            current_scale: None,
            config,
        }
    }

    /// Check if a scale is currently connected
    pub fn is_scale_connected(&self) -> bool {
        self.current_scale
            .as_ref()
            .map_or(false, |scale| scale.is_connected())
    }

    /// Get information about the currently connected scale
    pub fn get_current_scale_info(&self) -> Option<String> {
        self.current_scale.as_ref().map(|scale| {
            let info = scale.get_info();
            format!("{} {}", info.brand, info.model)
        })
    }

    /// Ensure a scale is connected, scanning if necessary
    pub async fn ensure_connected(&mut self) -> Result<(), ScanError> {
        if self.is_scale_connected() {
            return Ok(());
        }

        info!("📡 No scale connected - starting scan...");
        match self.scanner.scan_and_connect_with_retry(
            self.config.max_retry_attempts,
            self.config.retry_delay_ms
        ).await {
            Ok(scale) => {
                info!("✅ Scale connected: {}", scale.get_info().brand);
                self.current_scale = Some(scale);
                Ok(())
            }
            Err(e) => {
                error!("❌ Scale connection failed: {:?}", e);
                Err(e)
            }
        }
    }

    /// Disconnect current scale
    pub async fn disconnect(&mut self) {
        if let Some(mut scale) = self.current_scale.take() {
            if let Err(e) = scale.disconnect().await {
                warn!("⚠️ Error during scale disconnect: {:?}", e);
            }
        }
    }

    /// Get the current scale reference (if connected)
    pub fn get_current_scale(&self) -> Option<&BookooScale> {
        self.current_scale.as_deref()
    }

    /// Get mutable reference to current scale (if connected)
    pub fn get_current_scale_mut(&mut self) -> Option<&mut BookooScale> {
        self.current_scale.as_deref_mut()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_scale_type_conversion() {
        assert_eq!(ScaleType::Bookoo.as_str(), "Bookoo");
    }

    #[test]
    fn test_scan_config_default() {
        let config = ScanConfig::default();
        assert_eq!(config.scan_timeout_ms, 10000);
        assert_eq!(config.max_retry_attempts, 3);
        assert_eq!(config.preferred_scale_types, vec![ScaleType::Bookoo]);
    }

    #[test]
    fn test_scale_manager_creation() {
        let data_channel = Arc::new(embassy_sync::channel::Channel::new());
        let status_channel = Arc::new(embassy_sync::channel::Channel::new());
        let config = ScanConfig::default();

        let manager = ScaleManager::new(data_channel, status_channel, config);
        assert!(!manager.is_scale_connected());
        assert!(manager.get_current_scale_info().is_none());
    }
}