//! Scale Detector Implementations
//! 
//! This module contains specific detector implementations for different
//! scale brands and models. Each detector knows how to identify and
//! connect to its corresponding scale type.

use crate::{
    ble::{Device, Service, StatusChannel, Uuid},
    scales::{
        bookoo::BookooScale,
        scanner::{AdvancedScaleDetector, ScaleDetector},
        traits::{ScaleDataChannel, SmartScale},
    },
};
use log::{debug, info, warn};
use std::sync::Arc;

/// Detector for Bookoo Themis Mini scales
/// 
/// Detects Bookoo scales by their characteristic device name prefix
/// and specific BLE service UUIDs. This is a high-priority detector
/// since it can definitively identify Bookoo devices.
#[derive(Debug)]
pub struct BookooScaleDetector;

impl ScaleDetector for BookooScaleDetector {
    fn can_handle_device(&self, device: &Device) -> bool {
        if let Some(ref name) = device.name {
            let is_bookoo = name.starts_with("BOOKOO_SC") || name.contains("BOOKOO");
            if is_bookoo {
                debug!("🎯 Bookoo scale detected by name: {}", name);
            }
            is_bookoo
        } else {
            false
        }
    }
    
    fn get_priority(&self) -> u8 {
        100 // Very high priority - Bookoo detection is definitive
    }
    
    fn get_scale_type_name(&self) -> &'static str {
        "Bookoo"
    }
    
    async fn create_scale_instance(
        &self,
        device: Device,
        data_channel: Arc<ScaleDataChannel>,
        status_channel: Arc<StatusChannel>,
    ) -> Result<Box<dyn SmartScale>, Box<dyn std::error::Error>> {
        info!("🏗️ Creating Bookoo scale instance for device: {:?}", device.name);
        
        // Create BookooScale instance with the discovered device
        let mut bookoo_scale = BookooScale::new(
            Arc::clone(&data_channel),
            Arc::clone(&status_channel),
        );
        
        // Connect to the specific device (this replaces the internal scanning)
        match bookoo_scale.connect_to_device(device).await {
            Ok(()) => {
                info!("✅ Successfully connected to Bookoo scale");
                Ok(Box::new(bookoo_scale))
            }
            Err(e) => {
                warn!("❌ Failed to connect to Bookoo scale: {:?}", e);
                Err(Box::new(e) as Box<dyn std::error::Error>)
            }
        }
    }
}

impl AdvancedScaleDetector for BookooScaleDetector {
    async fn can_handle_services(&self, services: &[Service]) -> bool {
        // Bookoo scales use these specific service UUIDs
        let bookoo_service_16bit = Uuid::from_u16(0x0FFE);
        let bookoo_service_128bit = Uuid::from_u128_bytes([
            0x00, 0x00, 0xFF, 0xE0, 0x00, 0x00, 0x10, 0x00,
            0x80, 0x00, 0x00, 0x80, 0x5F, 0x9B, 0x34, 0xFB
        ]);
        
        let has_bookoo_service = services.iter().any(|service| {
            service.uuid == bookoo_service_16bit || service.uuid == bookoo_service_128bit
        });
        
        if has_bookoo_service {
            debug!("✅ Bookoo service UUID detected in service list");
        }
        
        has_bookoo_service
    }
    
    fn get_identifying_service_uuids(&self) -> Vec<Uuid> {
        vec![
            Uuid::from_u16(0x0FFE),
            Uuid::from_u128_bytes([
                0x00, 0x00, 0xFF, 0xE0, 0x00, 0x00, 0x10, 0x00,
                0x80, 0x00, 0x00, 0x80, 0x5F, 0x9B, 0x34, 0xFB
            ]),
        ]
    }
}

/// Example detector for Acaia scales (template for future implementation)
/// 
/// This detector shows how to add support for additional scale brands.
/// Acaia scales typically have different naming patterns and service UUIDs.
#[derive(Debug)]
pub struct AcaiaScaleDetector;

impl ScaleDetector for AcaiaScaleDetector {
    fn can_handle_device(&self, device: &Device) -> bool {
        if let Some(ref name) = device.name {
            let is_acaia = name.starts_with("ACAIA") || 
                          name.contains("ACAIA") ||
                          name.starts_with("PYXIS") ||  // Acaia Pyxis series
                          name.starts_with("LUNAR");    // Acaia Lunar series
            if is_acaia {
                debug!("🎯 Acaia scale detected by name: {}", name);
            }
            is_acaia
        } else {
            false
        }
    }
    
    fn get_priority(&self) -> u8 {
        90 // High priority but lower than Bookoo since we don't have implementation yet
    }
    
    fn get_scale_type_name(&self) -> &'static str {
        "Acaia"
    }
    
    async fn create_scale_instance(
        &self,
        _device: Device,
        _data_channel: Arc<ScaleDataChannel>,
        _status_channel: Arc<StatusChannel>,
    ) -> Result<Box<dyn SmartScale>, Box<dyn std::error::Error>> {
        // TODO: Implement AcaiaScale when we have the protocol implementation
        Err("Acaia scale support not yet implemented".into())
    }
}

impl AdvancedScaleDetector for AcaiaScaleDetector {
    async fn can_handle_services(&self, services: &[Service]) -> bool {
        // Acaia scales typically use different service UUIDs
        // This would need to be determined from Acaia protocol documentation
        let acaia_service_uuid = Uuid::from_u128_bytes([
            // Placeholder UUID - would need actual Acaia service UUID
            0x49, 0x53, 0x43, 0x41, 0x49, 0x41, 0x00, 0x00,
            0x10, 0x00, 0x80, 0x00, 0x00, 0x80, 0x5F, 0x9B
        ]);
        
        services.iter().any(|service| service.uuid == acaia_service_uuid)
    }
    
    fn get_identifying_service_uuids(&self) -> Vec<Uuid> {
        vec![
            // Placeholder - would need actual Acaia service UUIDs
            Uuid::from_u128_bytes([
                0x49, 0x53, 0x43, 0x41, 0x49, 0x41, 0x00, 0x00,
                0x10, 0x00, 0x80, 0x00, 0x00, 0x80, 0x5F, 0x9B
            ]),
        ]
    }
}

/// Generic/fallback detector for unknown scales
/// 
/// This detector attempts to identify scales that expose standard
/// weight measurement services but aren't specifically recognized.
/// It has the lowest priority and should only be used as a last resort.
#[derive(Debug)]
pub struct GenericScaleDetector;

impl ScaleDetector for GenericScaleDetector {
    fn can_handle_device(&self, device: &Device) -> bool {
        if let Some(ref name) = device.name {
            // Look for scale-like keywords in device names
            let scale_keywords = ["SCALE", "WEIGHT", "COFFEE", "BREWING", "GRAM"];
            let name_upper = name.to_uppercase();
            
            let is_likely_scale = scale_keywords.iter().any(|keyword| {
                name_upper.contains(keyword)
            });
            
            if is_likely_scale {
                debug!("🤔 Potential generic scale detected by name: {}", name);
            }
            
            is_likely_scale
        } else {
            false
        }
    }
    
    fn get_priority(&self) -> u8 {
        10 // Very low priority - only use if no specific detector matches
    }
    
    fn get_scale_type_name(&self) -> &'static str {
        "Generic"
    }
    
    async fn create_scale_instance(
        &self,
        _device: Device,
        _data_channel: Arc<ScaleDataChannel>,
        _status_channel: Arc<StatusChannel>,
    ) -> Result<Box<dyn SmartScale>, Box<dyn std::error::Error>> {
        // TODO: Implement generic scale protocol detection and connection
        Err("Generic scale support not yet implemented".into())
    }
}

impl AdvancedScaleDetector for GenericScaleDetector {
    async fn can_handle_services(&self, services: &[Service]) -> bool {
        // Look for standard BLE services that might indicate a scale
        // These are common UUIDs for weight/health devices
        let weight_service_uuids = [
            // Weight Scale Service (0x181D)
            Uuid::from_u16(0x181D),
            // Health Thermometer Service (0x1809) 
            Uuid::from_u16(0x1809),
            // Battery Service (0x180F)
            Uuid::from_u16(0x180F),
        ];
        
        let has_weight_services = services.iter().any(|service| {
            weight_service_uuids.contains(&service.uuid)
        });
        
        if has_weight_services {
            debug!("🔍 Standard weight service detected - might be generic scale");
        }
        
        has_weight_services
    }
    
    fn get_identifying_service_uuids(&self) -> Vec<Uuid> {
        vec![
            // Weight Scale Service (0x181D)
            Uuid::from_u16(0x181D),
            // Health Thermometer Service (0x1809)
            Uuid::from_u16(0x1809),
            // Battery Service (0x180F)
            Uuid::from_u16(0x180F),
        ]
    }
}

/// Helper function to create all available scale detectors
/// 
/// This function returns a vector of all implemented scale detectors,
/// ready to be registered with a ScaleScanner. Add new detectors here
/// as they are implemented.
pub fn create_all_scale_detectors() -> Vec<Box<dyn ScaleDetector>> {
    vec![
        Box::new(BookooScaleDetector),
        // TODO: Uncomment when Acaia implementation is ready
        // Box::new(AcaiaScaleDetector),
        // TODO: Uncomment when generic scale support is implemented
        // Box::new(GenericScaleDetector),
    ]
}

/// Helper function to create detectors for a specific set of scale types
pub fn create_scale_detectors(scale_types: &[&str]) -> Vec<Box<dyn ScaleDetector>> {
    let mut detectors = Vec::new();
    
    for scale_type in scale_types {
        match *scale_type {
            "Bookoo" => detectors.push(Box::new(BookooScaleDetector)),
            "Acaia" => detectors.push(Box::new(AcaiaScaleDetector)),
            "Generic" => detectors.push(Box::new(GenericScaleDetector)),
            unknown => {
                warn!("Unknown scale type requested: {}", unknown);
            }
        }
    }
    
    detectors
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_bookoo_detector_priority() {
        let detector = BookooScaleDetector;
        assert_eq!(detector.get_priority(), 100);
        assert_eq!(detector.get_scale_type_name(), "Bookoo");
    }
    
    #[test]
    fn test_bookoo_name_detection() {
        let detector = BookooScaleDetector;
        
        // Test various Bookoo device name patterns
        let bookoo_device = Device {
            name: Some("BOOKOO_SC_001".to_string()),
            address: [0; 6],
        };
        assert!(detector.can_handle_device(&bookoo_device));
        
        let bookoo_device2 = Device {
            name: Some("BOOKOO Mini".to_string()),
            address: [0; 6],
        };
        assert!(detector.can_handle_device(&bookoo_device2));
        
        // Test non-Bookoo device
        let other_device = Device {
            name: Some("ACAIA_LUNAR".to_string()),
            address: [0; 6],
        };
        assert!(!detector.can_handle_device(&other_device));
    }
    
    #[test]
    fn test_detector_creation() {
        let detectors = create_all_scale_detectors();
        assert!(!detectors.is_empty());
        
        let specific_detectors = create_scale_detectors(&["Bookoo"]);
        assert_eq!(specific_detectors.len(), 1);
        assert_eq!(specific_detectors[0].get_scale_type_name(), "Bookoo");
    }
}