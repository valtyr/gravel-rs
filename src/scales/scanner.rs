//! Generic BLE Scale Scanner with Multi-Brand Detection
//! 
//! This module provides a generic BLE scanner that can detect and connect to
//! different types of smart scales automatically. It uses a detector pattern
//! to identify scale brands and models, then hands off to the appropriate
//! scale implementation.

use crate::{
    ble::{BleClient, Device, DeviceFilter, Service},
    scales::traits::{ScaleDataChannel, SmartScale},
    ble::StatusChannel,
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

/// Trait for detecting and creating scale instances from BLE devices
/// 
/// Each scale brand/model should implement this trait to provide
/// detection logic and scale instantiation.
pub trait ScaleDetector: Send + Sync {
    /// Check if this detector can handle the given BLE device
    /// This is called during the initial device discovery phase
    fn can_handle_device(&self, device: &Device) -> bool;
    
    /// Get the priority of this detector (higher = more specific)
    /// Used to order detectors when multiple can handle the same device
    fn get_priority(&self) -> u8;
    
    /// Get a human-readable name for this scale type
    fn get_scale_type_name(&self) -> &'static str;
    
    /// Create a scale instance for the detected device
    /// This should handle full connection setup and return a ready-to-use scale
    async fn create_scale_instance(
        &self, 
        device: Device,
        data_channel: Arc<ScaleDataChannel>,
        status_channel: Arc<StatusChannel>
    ) -> Result<Box<dyn SmartScale>, Box<dyn std::error::Error>>;
}

/// Advanced detector trait for scales that need service-level detection
/// 
/// Some scales can't be reliably identified by name alone and need
/// to be detected based on their BLE services and characteristics.
pub trait AdvancedScaleDetector: ScaleDetector {
    /// Check if this detector can handle devices with the given services
    /// This is called after connecting and discovering services
    async fn can_handle_services(&self, services: &[Service]) -> bool;
    
    /// Get service UUIDs that uniquely identify this scale type
    /// Used for optimized scanning when looking for specific scales
    fn get_identifying_service_uuids(&self) -> Vec<uuid::Uuid>;
}

/// Generic BLE scale scanner that can detect multiple scale types
/// 
/// The scanner maintains a registry of scale detectors and uses them
/// to identify and connect to compatible scales automatically.
pub struct ScaleScanner {
    ble_client: BleClient,
    detectors: Vec<Box<dyn ScaleDetector>>,
    data_channel: Arc<ScaleDataChannel>,
    status_channel: Arc<StatusChannel>,
    scan_timeout_ms: u32,
    connection_timeout_ms: u32,
}

impl ScaleScanner {
    /// Create a new scale scanner with the given communication channels
    pub fn new(
        data_channel: Arc<ScaleDataChannel>,
        status_channel: Arc<StatusChannel>,
    ) -> Self {
        Self {
            ble_client: BleClient::new(),
            detectors: Vec::new(),
            data_channel,
            status_channel,
            scan_timeout_ms: 10000,  // 10 seconds default scan
            connection_timeout_ms: 5000,  // 5 seconds connection timeout
        }
    }
    
    /// Register a scale detector with the scanner
    pub fn register_detector(&mut self, detector: Box<dyn ScaleDetector>) {
        info!("🔍 Registered scale detector: {}", detector.get_scale_type_name());
        self.detectors.push(detector);
        
        // Sort detectors by priority (highest first) after each registration
        self.detectors.sort_by_key(|d| std::cmp::Reverse(d.get_priority()));
    }
    
    /// Set scan timeout in milliseconds
    pub fn set_scan_timeout(&mut self, timeout_ms: u32) {
        self.scan_timeout_ms = timeout_ms;
    }
    
    /// Set connection timeout in milliseconds
    pub fn set_connection_timeout(&mut self, timeout_ms: u32) {
        self.connection_timeout_ms = timeout_ms;
    }
    
    /// Get list of registered scale types
    pub fn get_registered_scale_types(&self) -> Vec<&'static str> {
        self.detectors.iter().map(|d| d.get_scale_type_name()).collect()
    }
    
    /// Scan for compatible scales and connect to the best one found
    /// 
    /// This method performs a comprehensive scan:
    /// 1. Discovers all nearby BLE devices
    /// 2. Evaluates each device against all registered detectors
    /// 3. Prioritizes detectors and attempts connection
    /// 4. Returns the first successfully connected scale
    pub async fn scan_and_connect(&mut self) -> Result<Box<dyn SmartScale>, ScanError> {
        if self.detectors.is_empty() {
            warn!("No scale detectors registered - cannot scan for scales");
            return Err(ScanError::NoCompatibleScales);
        }
        
        info!("🔍 Starting generic scale scan with {} detectors: {:?}", 
              self.detectors.len(), 
              self.get_registered_scale_types());
        
        // Phase 1: Broad BLE device discovery
        let devices = self.discover_ble_devices().await?;
        if devices.is_empty() {
            return Err(ScanError::NoDevicesFound);
        }
        
        info!("📡 Found {} BLE devices, evaluating compatibility...", devices.len());
        
        // Phase 2: Evaluate devices against detectors
        let candidates = self.evaluate_device_candidates(devices).await;
        if candidates.is_empty() {
            warn!("❌ No compatible scales found among discovered devices");
            return Err(ScanError::NoCompatibleScales);
        }
        
        info!("✅ Found {} compatible scale candidates", candidates.len());
        
        // Phase 3: Attempt connection to best candidate
        self.connect_to_best_candidate(candidates).await
    }
    
    /// Scan for a specific scale type by name
    pub async fn scan_for_scale_type(&mut self, scale_type: &str) -> Result<Box<dyn SmartScale>, ScanError> {
        let detector = self.detectors.iter()
            .find(|d| d.get_scale_type_name() == scale_type)
            .ok_or_else(|| ScanError::BleError(format!("Unknown scale type: {}", scale_type)))?;
        
        info!("🎯 Scanning specifically for {} scales", scale_type);
        
        let devices = self.discover_ble_devices().await?;
        
        for device in devices {
            if detector.can_handle_device(&device) {
                info!("📱 Found {} device: {:?}", scale_type, device.name);
                
                match detector.create_scale_instance(
                    device,
                    Arc::clone(&self.data_channel),
                    Arc::clone(&self.status_channel)
                ).await {
                    Ok(scale) => {
                        info!("✅ Successfully connected to {} scale", scale_type);
                        return Ok(scale);
                    }
                    Err(e) => {
                        warn!("❌ Failed to connect to {} scale: {:?}", scale_type, e);
                        continue;
                    }
                }
            }
        }
        
        Err(ScanError::NoCompatibleScales)
    }
    
    /// Discover BLE devices without filtering
    async fn discover_ble_devices(&mut self) -> Result<Vec<Device>, ScanError> {
        debug!("🔍 Starting BLE device discovery ({}ms timeout)", self.scan_timeout_ms);
        
        // Use no filter to discover all devices
        let filter = DeviceFilter {
            name_prefix: None,
            service_uuid: None,
        };
        
        match self.ble_client.scan_for_devices(Some(filter), self.scan_timeout_ms).await {
            Ok(devices) => {
                debug!("📡 Discovered {} BLE devices", devices.len());
                for device in &devices {
                    debug!("  - Device: {:?} (Address: {:?})", device.name, device.address);
                }
                Ok(devices)
            }
            Err(e) => {
                error!("🚨 BLE device discovery failed: {:?}", e);
                Err(ScanError::BleError(format!("Discovery failed: {:?}", e)))
            }
        }
    }
    
    /// Evaluate discovered devices against all registered detectors
    async fn evaluate_device_candidates(&self, devices: Vec<Device>) -> Vec<ScaleCandidate> {
        let mut candidates = Vec::new();
        
        for device in devices {
            // Test device against all detectors
            for detector in &self.detectors {
                if detector.can_handle_device(&device) {
                    let candidate = ScaleCandidate {
                        device: device.clone(),
                        detector_index: candidates.len(), // Will be replaced with proper index
                        priority: detector.get_priority(),
                        scale_type: detector.get_scale_type_name(),
                    };
                    
                    debug!("✅ Device {} compatible with {} detector (priority: {})", 
                           device.name.as_deref().unwrap_or("Unknown"),
                           candidate.scale_type,
                           candidate.priority);
                    
                    candidates.push(candidate);
                    break; // Use first compatible detector (they're priority-sorted)
                }
            }
        }
        
        // Sort candidates by priority (highest first)
        candidates.sort_by_key(|c| std::cmp::Reverse(c.priority));
        
        // Update detector indices after sorting
        for (i, candidate) in candidates.iter_mut().enumerate() {
            candidate.detector_index = self.detectors.iter()
                .position(|d| d.get_scale_type_name() == candidate.scale_type)
                .unwrap_or(0);
        }
        
        candidates
    }
    
    /// Attempt to connect to the best scale candidate
    async fn connect_to_best_candidate(&self, candidates: Vec<ScaleCandidate>) -> Result<Box<dyn SmartScale>, ScanError> {
        for candidate in candidates {
            let detector = &self.detectors[candidate.detector_index];
            
            info!("🔗 Attempting to connect to {} scale: {:?}", 
                  candidate.scale_type,
                  candidate.device.name);
            
            match detector.create_scale_instance(
                candidate.device.clone(),
                Arc::clone(&self.data_channel),
                Arc::clone(&self.status_channel)
            ).await {
                Ok(scale) => {
                    info!("🎉 Successfully connected to {} scale!", candidate.scale_type);
                    return Ok(scale);
                }
                Err(e) => {
                    warn!("❌ Failed to connect to {} scale: {:?} - trying next candidate...", 
                          candidate.scale_type, e);
                    
                    // Brief delay before trying next candidate
                    Timer::after(Duration::from_millis(500)).await;
                    continue;
                }
            }
        }
        
        error!("💀 All scale connection attempts failed");
        Err(ScanError::ConnectionFailed("All candidates failed".to_string()))
    }
}

/// Represents a scale device that can potentially be connected to
#[derive(Debug, Clone)]
struct ScaleCandidate {
    device: Device,
    detector_index: usize,
    priority: u8,
    scale_type: &'static str,
}

/// Configuration for scale scanning behavior
#[derive(Debug, Clone)]
pub struct ScanConfig {
    pub scan_timeout_ms: u32,
    pub connection_timeout_ms: u32,
    pub preferred_scale_types: Vec<String>,
    pub retry_attempts: u32,
    pub retry_delay_ms: u32,
}

impl Default for ScanConfig {
    fn default() -> Self {
        Self {
            scan_timeout_ms: 10000,
            connection_timeout_ms: 5000,
            preferred_scale_types: vec!["Bookoo".to_string()],
            retry_attempts: 3,
            retry_delay_ms: 2000,
        }
    }
}

impl ScaleScanner {
    /// Apply configuration to the scanner
    pub fn configure(&mut self, config: &ScanConfig) {
        self.scan_timeout_ms = config.scan_timeout_ms;
        self.connection_timeout_ms = config.connection_timeout_ms;
        
        // Reorder detectors based on preferences
        if !config.preferred_scale_types.is_empty() {
            self.detectors.sort_by_key(|d| {
                if let Some(pos) = config.preferred_scale_types.iter()
                    .position(|pref| pref == d.get_scale_type_name()) {
                    // Preferred scales get higher priority
                    (std::cmp::Reverse(pos), std::cmp::Reverse(d.get_priority()))
                } else {
                    // Non-preferred scales use original priority
                    (std::cmp::Reverse(config.preferred_scale_types.len()), std::cmp::Reverse(d.get_priority()))
                }
            });
        }
        
        info!("⚙️ Scanner configured: timeout={}ms, preferences={:?}", 
              config.scan_timeout_ms, 
              config.preferred_scale_types);
    }
    
    /// Scan with retry logic based on configuration
    pub async fn scan_and_connect_with_retry(&mut self, config: &ScanConfig) -> Result<Box<dyn SmartScale>, ScanError> {
        self.configure(config);
        
        for attempt in 1..=config.retry_attempts {
            info!("🔄 Scale scan attempt {}/{}", attempt, config.retry_attempts);
            
            match self.scan_and_connect().await {
                Ok(scale) => {
                    info!("✅ Scale connection successful on attempt {}", attempt);
                    return Ok(scale);
                }
                Err(e) => {
                    if attempt < config.retry_attempts {
                        warn!("❌ Scan attempt {} failed: {:?} - retrying in {}ms", 
                              attempt, e, config.retry_delay_ms);
                        Timer::after(Duration::from_millis(config.retry_delay_ms as u64)).await;
                    } else {
                        error!("💀 All {} scan attempts failed: {:?}", config.retry_attempts, e);
                        return Err(e);
                    }
                }
            }
        }
        
        Err(ScanError::Timeout)
    }
}