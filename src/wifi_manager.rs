//! WiFi management for both provisioning and normal station operation

use crate::wifi_provisioning::WifiProvisioning;
use esp_idf_svc::wifi::{EspWifi, BlockingWifi, Configuration, ClientConfiguration, AccessPointConfiguration};
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::hal::modem::Modem;
use esp_idf_svc::sys::EspError;
use log::{info, warn, error, debug};
use embassy_time::{Duration, Timer};

pub struct WifiManager {
    wifi: Option<BlockingWifi<EspWifi<'static>>>,
    provisioning: Option<WifiProvisioning>,
    is_provisioned: bool,
}

impl WifiManager {
    /// Initialize WiFi manager with provisioning capability
    pub async fn new(
        modem: Modem,
        sys_loop: EspSystemEventLoop,
        nvs: EspDefaultNvsPartition,
    ) -> Result<Self, EspError> {
        info!("🌐 Initializing WiFi Manager");

        // Initialize basic WiFi driver  
        let wifi = EspWifi::new(modem, sys_loop.clone(), Some(nvs))?;
        let wifi = BlockingWifi::wrap(wifi, sys_loop)?;

        // Initialize provisioning
        let provisioning = match WifiProvisioning::new() {
            Ok(prov) => {
                info!("📲 WiFi provisioning initialized");
                Some(prov)
            }
            Err(e) => {
                warn!("⚠️ WiFi provisioning failed to initialize: {:?}", e);
                None
            }
        };

        // Check if already provisioned
        let is_provisioned = provisioning
            .as_ref()
            .map(|p| {
                match p.is_provisioned() {
                    Ok(provisioned) => {
                        info!("📋 WiFi provisioning check result: {}", provisioned);
                        provisioned
                    }
                    Err(e) => {
                        warn!("⚠️ Error checking provisioning status: {:?}", e);
                        false
                    }
                }
            })
            .unwrap_or(false);

        info!("📋 WiFi provisioning final status: {}", 
              if is_provisioned { "provisioned" } else { "not provisioned" });

        Ok(WifiManager {
            wifi: Some(wifi),
            provisioning,
            is_provisioned,
        })
    }

    /// Start WiFi - either connect to stored network or start provisioning
    /// Returns (success, ble_stack_needs_reset)  
    pub async fn start(&mut self) -> Result<(bool, bool), EspError> {
        if let Some(ref provisioning) = self.provisioning {
            // Implement dice-style provisioning loop
            loop {
                // Check current provisioning status
                let is_provisioned = provisioning.is_provisioned().unwrap_or(false);
                info!("📋 Provisioning status check: {}", is_provisioned);
                
                if !is_provisioned {
                    info!("🔧 Starting WiFi provisioning mode");
                    
                    // Set WiFi to client mode first (like dice example)
                    if let Some(ref mut wifi) = self.wifi {
                        let wifi_configuration = Configuration::Client(ClientConfiguration::default());
                        wifi.set_configuration(&wifi_configuration)?;
                        wifi.start()?;
                    }
                    
                    // Generate unique device name
                    let device_name = WifiProvisioning::generate_device_name("GravelScale");
                    let pop = Some("gravel123");
                    
                    info!("🚀 Starting BLE provisioning as '{}'", device_name);
                    provisioning.start_provisioning(&device_name, pop, None)?;
                    
                    // Wait for provisioning to complete
                    provisioning.wait_for_provisioning()?;
                    
                    info!("🎉 WiFi provisioning completed!");
                    provisioning.stop_provisioning();
                    
                    // Give time for BLE stack cleanup
                    Timer::after(Duration::from_millis(2000)).await;
                    
                    // Try to connect after provisioning
                    if let Some(ref mut wifi) = self.wifi {
                        match wifi.wait_netif_up() {
                            Ok(_) => {
                                info!("✅ WiFi connected successfully after provisioning");
                                return Ok((true, true)); // Connected, BLE needs reset
                            }
                            Err(e) => {
                                warn!("⚠️ Failed to connect after provisioning: {:?}", e);
                                // Reset provisioning and try again (like dice)
                                info!("🔄 Resetting provisioning to try again");
                                provisioning.reset_provisioning().ok();
                                continue;
                            }
                        }
                    }
                } else {
                    info!("📶 Already provisioned - attempting connection");
                    
                    if let Some(ref mut wifi) = self.wifi {
                        // DON'T set configuration for stored credentials - let ESP-IDF use stored ones
                        // Only set default config for fresh provisioning
                        wifi.start()?;
                        
                        // Small delay to let WiFi stack settle
                        Timer::after(Duration::from_millis(1000)).await;
                        
                        match wifi.connect() {
                            Ok(_) => {
                                match wifi.wait_netif_up() {
                                    Ok(_) => {
                                        info!("✅ Connected to stored WiFi successfully");
                                        return Ok((true, false)); // Connected, no BLE reset needed
                                    }
                                    Err(e) => {
                                        warn!("❌ Failed to get IP with stored credentials: {:?}", e);
                                        // Reset provisioning and try again (like dice)
                                        provisioning.reset_provisioning().ok();
                                        continue;
                                    }
                                }
                            }
                            Err(e) => {
                                warn!("❌ Failed to connect with stored credentials: {:?}", e);
                                // Only reset on certain errors - not timeouts
                                if matches!(e.code(), esp_idf_svc::sys::ESP_ERR_WIFI_SSID | esp_idf_svc::sys::ESP_ERR_WIFI_PASSWORD) {
                                    warn!("🔄 Bad credentials detected - resetting provisioning");
                                    provisioning.reset_provisioning().ok();
                                } else {
                                    warn!("🔄 Network issue, retrying without reset");
                                    Timer::after(Duration::from_millis(5000)).await;
                                }
                                continue;
                            }
                        }
                    }
                }
            }
        } else {
            warn!("⚠️ WiFi provisioning not available");
            Ok((false, false))
        }
    }

    /// Connect to WiFi after provisioning (more aggressive retry)
    async fn connect_after_provisioning(&mut self) -> Result<(), EspError> {
        if let Some(ref mut wifi) = self.wifi {
            // Set to station mode
            wifi.set_configuration(&Configuration::Client(ClientConfiguration::default()))?;
            wifi.start()?;

            info!("🔌 Connecting to WiFi with provisioned credentials");
            
            // More aggressive connection attempt for provisioning
            for attempt in 1..=3 {
                match wifi.connect() {
                    Ok(_) => {
                        info!("📡 WiFi connect call succeeded (attempt {})", attempt);
                        
                        // Wait for IP with longer timeout
                        match wifi.wait_netif_up() {
                            Ok(_) => {
                                info!("🌐 WiFi connected successfully with IP address");
                                return Ok(());
                            }
                            Err(e) => {
                                warn!("❌ Failed to get IP address (attempt {}): {:?}", attempt, e);
                                if attempt < 3 {
                                    Timer::after(Duration::from_millis(2000)).await;
                                }
                            }
                        }
                    }
                    Err(e) => {
                        error!("❌ WiFi connect failed (attempt {}): {:?}", attempt, e);
                        if attempt < 3 {
                            Timer::after(Duration::from_millis(2000)).await;
                        }
                    }
                }
            }
            
            Err(EspError::from(esp_idf_svc::sys::ESP_ERR_WIFI_CONN).unwrap())
        } else {
            Err(EspError::from(esp_idf_svc::sys::ESP_ERR_INVALID_STATE).unwrap())
        }
    }

    /// Connect to stored WiFi network
    async fn connect_to_stored_network(&mut self) -> Result<(), EspError> {
        if let Some(ref mut wifi) = self.wifi {
            // Set to station mode
            wifi.set_configuration(&Configuration::Client(ClientConfiguration::default()))?;
            wifi.start()?;

            info!("🔌 Attempting to connect to stored WiFi credentials");
            
            // Try to connect - this will use stored credentials
            match wifi.connect() {
                Ok(_) => {
                    info!("✅ Connected to WiFi successfully");
                    
                    // Wait for IP
                    wifi.wait_netif_up()?;
                    info!("🌐 WiFi connected with IP address");
                    
                    Ok(())
                }
                Err(e) => {
                    error!("❌ Failed to connect to stored WiFi: {:?}", e);
                    warn!("🔄 WiFi connection failed - continuing without WiFi");
                    
                    // For non-provisioning calls, continue without WiFi
                    // For provisioning calls, this will be handled differently
                    Ok(())
                }
            }
        } else {
            Err(EspError::from(esp_idf_svc::sys::ESP_ERR_INVALID_STATE).unwrap())
        }
    }

    /// Start BLE provisioning mode
    async fn start_provisioning_mode(&mut self) -> Result<(), EspError> {
        if let Some(ref provisioning) = self.provisioning {
            // Set WiFi to client mode first (like dice example)
            if let Some(ref mut wifi) = self.wifi {
                let wifi_configuration = Configuration::Client(ClientConfiguration::default());
                wifi.set_configuration(&wifi_configuration)?;
                wifi.start()?;
            }
            
            // Generate unique device name
            let device_name = WifiProvisioning::generate_device_name("GravelScale");
            
            // Start provisioning with security (you can customize the POP)
            let pop = Some("gravel123"); // Proof of Possession - customize this
            
            info!("🚀 Starting BLE provisioning as '{}'", device_name);
            provisioning.start_provisioning(&device_name, pop, None)?;
            
            // Wait for provisioning to complete using ESP-IDF built-in wait
            provisioning.wait_for_provisioning()?;
            
            info!("🎉 WiFi provisioning completed!");
            self.is_provisioned = true;
            
            // Stop provisioning service
            provisioning.stop_provisioning();
            
            // Give time for BLE stack cleanup
            Timer::after(Duration::from_millis(2000)).await;
            
            // Now wait for network interface to come up (like dice example)
            if let Some(ref mut wifi) = self.wifi {
                info!("🔌 Waiting for WiFi network interface after provisioning");
                match wifi.wait_netif_up() {
                    Ok(_) => {
                        info!("✅ WiFi connected successfully after provisioning");
                        Ok(())
                    }
                    Err(e) => {
                        warn!("⚠️ Failed to get IP after provisioning: {:?} - continuing without WiFi", e);
                        Ok(())
                    }
                }
            } else {
                warn!("⚠️ WiFi not available after provisioning");
                Ok(())
            }
        } else {
            warn!("⚠️ WiFi provisioning not available - continuing without WiFi");
            Ok(())
        }
    }

    /// Check if WiFi is connected
    pub fn is_connected(&self) -> bool {
        self.wifi
            .as_ref()
            .map(|w| w.is_connected().unwrap_or(false))
            .unwrap_or(false)
    }

    /// Get WiFi status for display
    pub fn get_status(&self) -> String {
        if self.is_connected() {
            "Connected".to_string()
        } else if self.is_provisioned {
            "Disconnected".to_string()
        } else {
            "Not Provisioned".to_string()
        }
    }

    /// Reset WiFi provisioning (for factory reset)
    pub fn reset_provisioning(&mut self) -> Result<(), EspError> {
        if let Some(ref provisioning) = self.provisioning {
            info!("🔄 Resetting WiFi provisioning");
            provisioning.reset_provisioning()?;
            self.is_provisioned = false;
            info!("✅ WiFi provisioning reset complete");
        }
        Ok(())
    }

    /// Attempt to reconnect to WiFi
    pub async fn reconnect(&mut self) -> Result<(), EspError> {
        if self.is_provisioned && !self.is_connected() {
            info!("🔄 Attempting WiFi reconnection");
            self.connect_to_stored_network().await
        } else {
            Ok(())
        }
    }

    /// Start a monitoring task that periodically checks connection
    pub async fn monitor_connection(&mut self) {
        let mut check_interval = Duration::from_secs(30); // Check every 30 seconds
        let mut consecutive_failures = 0;
        
        loop {
            Timer::after(check_interval).await;
            
            if self.is_provisioned && !self.is_connected() {
                consecutive_failures += 1;
                warn!("📡 WiFi disconnected (failure #{}/3)", consecutive_failures);
                
                if consecutive_failures <= 3 {
                    if let Err(e) = self.reconnect().await {
                        error!("❌ WiFi reconnection failed: {:?}", e);
                    } else {
                        info!("✅ WiFi reconnected successfully");
                        consecutive_failures = 0;
                        check_interval = Duration::from_secs(30); // Reset to normal interval
                    }
                } else {
                    warn!("⚠️ WiFi reconnection failed 3 times - extending check interval");
                    check_interval = Duration::from_secs(300); // Check every 5 minutes after failures
                }
            } else {
                consecutive_failures = 0;
                check_interval = Duration::from_secs(30); // Normal interval when connected
            }
        }
    }
}