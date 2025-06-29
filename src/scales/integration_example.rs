//! Example integration showing how to use the generic scale scanner
//! 
//! This file demonstrates how to integrate the ScaleScanner into the
//! main controller for automatic scale detection and connection.

#[allow(dead_code)]
mod integration_example {
    use crate::{
        ble::StatusChannel,
        scales::{
            detectors::{create_all_scale_detectors, BookooScaleDetector},
            scanner::{ScanConfig, ScaleScanner},
            traits::{ScaleDataChannel, ScaleCommandChannel, SmartScale},
        },
    };
    use embassy_time::{Duration, Timer};
    use log::{error, info, warn};
    use std::sync::Arc;

    /// Example of how to integrate the ScaleScanner into the main controller
    pub struct ScaleManager {
        scanner: ScaleScanner,
        current_scale: Option<Box<dyn SmartScale>>,
        command_channel: Arc<ScaleCommandChannel>,
    }

    impl ScaleManager {
        /// Create a new scale manager with all available detectors
        pub fn new(
            data_channel: Arc<ScaleDataChannel>,
            status_channel: Arc<StatusChannel>,
            command_channel: Arc<ScaleCommandChannel>,
        ) -> Self {
            let mut scanner = ScaleScanner::new(data_channel, status_channel);

            // Register all available scale detectors
            let detectors = create_all_scale_detectors();
            for detector in detectors {
                scanner.register_detector(detector);
            }

            // Configure scanner with reasonable defaults
            let config = ScanConfig {
                scan_timeout_ms: 15000, // 15 seconds
                connection_timeout_ms: 8000, // 8 seconds
                preferred_scale_types: vec!["Bookoo".to_string()],
                retry_attempts: 3,
                retry_delay_ms: 5000, // 5 seconds between retries
            };
            scanner.configure(&config);

            info!("🔍 ScaleManager initialized with detectors: {:?}", 
                  scanner.get_registered_scale_types());

            Self {
                scanner,
                current_scale: None,
                command_channel,
            }
        }

        /// Create a scale manager with only specific scale types
        pub fn new_with_specific_types(
            data_channel: Arc<ScaleDataChannel>,
            status_channel: Arc<StatusChannel>,
            command_channel: Arc<ScaleCommandChannel>,
            scale_types: &[&str],
        ) -> Self {
            let mut scanner = ScaleScanner::new(data_channel, status_channel);

            // Register only specific detectors
            for scale_type in scale_types {
                match *scale_type {
                    "Bookoo" => scanner.register_detector(Box::new(BookooScaleDetector)),
                    // Add other scale types as they become available
                    unknown => warn!("⚠️ Unknown scale type requested: {}", unknown),
                }
            }

            Self {
                scanner,
                current_scale: None,
                command_channel,
            }
        }

        /// Start the scale management task - this would be spawned as an Embassy task
        pub async fn run_scale_management_task(&mut self) {
            info!("🚀 Starting scale management task");

            loop {
                if self.current_scale.is_none() {
                    info!("📡 No scale connected - starting scan...");

                    match self.scan_and_connect_with_retry().await {
                        Ok(scale) => {
                            info!("✅ Scale connected: {}", scale.get_info().brand);
                            self.current_scale = Some(scale);

                            // Start the scale monitoring in the background
                            if let Some(ref mut scale) = self.current_scale {
                                tokio::spawn(async move {
                                    // Note: In real implementation, this would use Embassy tasks
                                    // and proper command channel integration
                                    if let Err(e) = scale
                                        .start_with_commands(
                                            Arc::new(embassy_sync::channel::Channel::new()),
                                            Arc::new(embassy_sync::channel::Channel::new()),
                                            Arc::new(embassy_sync::channel::Channel::new()),
                                        )
                                        .await
                                    {
                                        error!("Scale monitoring failed: {:?}", e);
                                    }
                                });
                            }
                        }
                        Err(e) => {
                            error!("❌ Scale connection failed: {:?}", e);
                            Timer::after(Duration::from_secs(10)).await;
                        }
                    }
                } else {
                    // Check if current scale is still connected
                    if let Some(ref scale) = self.current_scale {
                        if !scale.is_connected() {
                            warn!("⚠️ Scale disconnected - will reconnect");
                            self.current_scale = None;
                        }
                    }

                    // Check every 5 seconds
                    Timer::after(Duration::from_secs(5)).await;
                }
            }
        }

        /// Scan and connect with retry logic
        async fn scan_and_connect_with_retry(&mut self) -> Result<Box<dyn SmartScale>, Box<dyn std::error::Error>> {
            let config = ScanConfig::default();
            self.scanner.scan_and_connect_with_retry(&config).await
                .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)
        }

        /// Force connection to a specific scale type
        pub async fn connect_to_scale_type(&mut self, scale_type: &str) -> Result<(), Box<dyn std::error::Error>> {
            info!("🎯 Forcing connection to {} scale", scale_type);

            // Disconnect current scale if any
            if let Some(mut scale) = self.current_scale.take() {
                scale.disconnect().await.ok();
            }

            // Scan for specific scale type
            match self.scanner.scan_for_scale_type(scale_type).await {
                Ok(scale) => {
                    info!("✅ Connected to {} scale", scale_type);
                    self.current_scale = Some(scale);
                    Ok(())
                }
                Err(e) => {
                    error!("❌ Failed to connect to {} scale: {:?}", scale_type, e);
                    Err(Box::new(e))
                }
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

        /// Get list of supported scale types
        pub fn get_supported_scale_types(&self) -> Vec<&'static str> {
            self.scanner.get_registered_scale_types()
        }
    }

    /// Example controller integration showing how the scanner replaces direct scale usage
    #[allow(dead_code)]
    pub struct ExampleController {
        scale_manager: ScaleManager,
        // ... other controller components
    }

    impl ExampleController {
        pub async fn new() -> Result<Self, Box<dyn std::error::Error>> {
            // Create communication channels
            let data_channel = Arc::new(embassy_sync::channel::Channel::new());
            let status_channel = Arc::new(embassy_sync::channel::Channel::new());
            let command_channel = Arc::new(embassy_sync::channel::Channel::new());

            // Create scale manager with automatic scale detection
            let scale_manager = ScaleManager::new(
                data_channel,
                status_channel,
                command_channel,
            );

            Ok(Self {
                scale_manager,
                // ... initialize other components
            })
        }

        /// Example Embassy task that would be spawned in the real controller
        pub async fn start_scale_management(&mut self) {
            // In real implementation, this would be:
            // spawner.spawn(scale_management_task(self.scale_manager)).unwrap();
            self.scale_manager.run_scale_management_task().await;
        }

        /// Handle user request to switch scale types
        pub async fn switch_to_scale_type(&mut self, scale_type: &str) -> Result<(), Box<dyn std::error::Error>> {
            self.scale_manager.connect_to_scale_type(scale_type).await
        }

        /// Get status for web interface
        pub fn get_scale_status(&self) -> ScaleStatus {
            ScaleStatus {
                connected: self.scale_manager.is_scale_connected(),
                scale_info: self.scale_manager.get_current_scale_info(),
                supported_types: self.scale_manager.get_supported_scale_types(),
            }
        }
    }

    /// Status information for web interface
    #[derive(Debug)]
    pub struct ScaleStatus {
        pub connected: bool,
        pub scale_info: Option<String>,
        pub supported_types: Vec<&'static str>,
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn test_scale_manager_creation() {
            let data_channel = Arc::new(embassy_sync::channel::Channel::new());
            let status_channel = Arc::new(embassy_sync::channel::Channel::new());
            let command_channel = Arc::new(embassy_sync::channel::Channel::new());

            let manager = ScaleManager::new(data_channel, status_channel, command_channel);

            assert!(!manager.get_supported_scale_types().is_empty());
            assert!(!manager.is_scale_connected());
        }

        #[test]
        fn test_specific_scale_types() {
            let data_channel = Arc::new(embassy_sync::channel::Channel::new());
            let status_channel = Arc::new(embassy_sync::channel::Channel::new());
            let command_channel = Arc::new(embassy_sync::channel::Channel::new());

            let manager = ScaleManager::new_with_specific_types(
                data_channel,
                status_channel,
                command_channel,
                &["Bookoo"],
            );

            let supported = manager.get_supported_scale_types();
            assert_eq!(supported.len(), 1);
            assert_eq!(supported[0], "Bookoo");
        }
    }
}