//! WiFi provisioning using ESP BLE Provisioning API
//! Allows users to configure WiFi credentials via BLE from a mobile app

use embassy_time::{Duration, Timer};
use esp_idf_svc::sys::*;
use std::ffi::{c_void, CString};
use std::ptr;

/// WiFi Provisioning Manager
pub struct WifiProvisioning {
    is_initialized: bool,
}

impl WifiProvisioning {
    /// Initialize the WiFi provisioning manager
    pub fn new() -> Result<Self, EspError> {
        ::log::info!("🔧 Initializing WiFi BLE provisioning");

        unsafe {
            // Configure WiFi provisioning with BLE scheme
            let config = wifi_prov_mgr_config_t {
                scheme: wifi_prov_scheme_ble, // Use BLE for provisioning
                scheme_event_handler: wifi_prov_event_handler_t {
                    event_cb: None, // No custom scheme callback
                    user_data: ptr::null_mut(),
                },
                app_event_handler: wifi_prov_event_handler_t {
                    event_cb: None, // No custom app callback
                    user_data: ptr::null_mut(),
                },
            };

            esp!(wifi_prov_mgr_init(config))?;
        }

        ::log::info!("✅ WiFi provisioning manager initialized");
        Ok(WifiProvisioning {
            is_initialized: true,
        })
    }

    /// Check if WiFi credentials are already provisioned
    pub fn is_provisioned(&self) -> Result<bool, EspError> {
        if !self.is_initialized {
            return Err(EspError::from(ESP_ERR_INVALID_STATE).unwrap());
        }

        let mut provisioned: bool = false;
        let result: esp_err_t = unsafe { wifi_prov_mgr_is_provisioned(&mut provisioned) };

        if result == ESP_OK {
            ::log::debug!(
                "📋 WiFi provisioning status: {}",
                if provisioned {
                    "provisioned"
                } else {
                    "not provisioned"
                }
            );
            Ok(provisioned)
        } else {
            Err(EspError::from(result).unwrap())
        }
    }

    /// Start BLE provisioning service
    pub fn start_provisioning(
        &self,
        device_name: &str,
        pop: Option<&str>,         // Proof of Possession (optional PIN)
        service_key: Option<&str>, // Optional service key
    ) -> Result<(), EspError> {
        if !self.is_initialized {
            return Err(EspError::from(ESP_ERR_INVALID_STATE).unwrap());
        }

        ::log::info!("🚀 Starting BLE provisioning service: '{}'", device_name);

        let device_name_cstr =
            CString::new(device_name).map_err(|_| EspError::from(ESP_ERR_INVALID_ARG).unwrap())?;

        let pop_cstr = pop
            .map(|p| CString::new(p))
            .transpose()
            .map_err(|_| EspError::from(ESP_ERR_INVALID_ARG).unwrap())?;

        let service_key_cstr = service_key
            .map(|k| CString::new(k))
            .transpose()
            .map_err(|_| EspError::from(ESP_ERR_INVALID_ARG).unwrap())?;

        // Use security level 1 (with optional POP) or 0 (no security)
        let security = if pop.is_some() {
            wifi_prov_security_WIFI_PROV_SECURITY_1
        } else {
            wifi_prov_security_WIFI_PROV_SECURITY_0
        };

        let pop_ptr = pop_cstr
            .as_ref()
            .map(|cstr| cstr.as_ptr() as *const c_void)
            .unwrap_or(ptr::null());

        unsafe {
            esp!(wifi_prov_mgr_start_provisioning(
                security,
                pop_ptr,
                device_name_cstr.as_ptr(),
                service_key_cstr
                    .as_ref()
                    .map(|cstr| cstr.as_ptr())
                    .unwrap_or(ptr::null()),
            ))?;
        }

        ::log::info!(
            "🔵 BLE provisioning active - device discoverable as '{}'",
            device_name
        );
        if let Some(pop_str) = pop {
            ::log::info!("🔐 Provisioning secured with POP: {}", pop_str);
        } else {
            ::log::warn!("⚠️ Provisioning running without security (no POP)");
        }

        // Generate and print QR code for easy pairing
        self.print_qr_code(device_name, pop);

        Ok(())
    }

    /// Wait for provisioning to complete (blocking) - uses ESP-IDF built-in wait
    pub fn wait_for_provisioning(&self) -> Result<(), EspError> {
        if !self.is_initialized {
            return Err(EspError::from(ESP_ERR_INVALID_STATE).unwrap());
        }

        ::log::info!("⏳ Waiting for WiFi provisioning (using built-in wait)");

        // Use ESP-IDF's built-in blocking wait - this handles everything properly
        unsafe {
            wifi_prov_mgr_wait();
        }

        ::log::info!("✅ WiFi provisioning completed successfully!");
        Ok(())
    }

    /// Stop the provisioning service
    pub fn stop_provisioning(&self) {
        if self.is_initialized {
            ::log::info!("🛑 Stopping WiFi provisioning service");
            unsafe {
                wifi_prov_mgr_stop_provisioning();
            }
        }
    }

    /// Reset WiFi provisioning (clear stored credentials)
    pub fn reset_provisioning(&self) -> Result<(), EspError> {
        if !self.is_initialized {
            return Err(EspError::from(ESP_ERR_INVALID_STATE).unwrap());
        }

        ::log::warn!("🔄 Resetting WiFi provisioning data");
        unsafe {
            esp!(wifi_prov_mgr_reset_provisioning())?;
        }

        ::log::info!("✅ WiFi provisioning data cleared");
        Ok(())
    }

    /// Get a user-friendly device name for provisioning
    pub fn generate_device_name(base_name: &str) -> String {
        // Use MAC address suffix for uniqueness
        let mac = Self::get_mac_suffix();
        format!("{}-{}", base_name, mac)
    }

    /// Get last 3 bytes of MAC address as hex string
    fn get_mac_suffix() -> String {
        unsafe {
            let mut mac = [0u8; 6];
            if esp_read_mac(mac.as_mut_ptr(), esp_mac_type_t_ESP_MAC_WIFI_STA) == ESP_OK {
                format!("{:02X}{:02X}{:02X}", mac[3], mac[4], mac[5])
            } else {
                "UNKNOWN".to_string()
            }
        }
    }

    /// Generate and print QR code for ESP BLE Provisioning app
    fn print_qr_code(&self, device_name: &str, pop: Option<&str>) {
        // ESP BLE Provisioning QR format:
        // {"ver":"v1","name":"DEVICE_NAME","pop":"POP_VALUE","transport":"ble"}

        let qr_payload = if let Some(pop_str) = pop {
            format!(
                r#"{{"ver":"v1","name":"{}","pop":"{}","transport":"ble"}}"#,
                device_name, pop_str
            )
        } else {
            format!(
                r#"{{"ver":"v1","name":"{}","transport":"ble"}}"#,
                device_name
            )
        };

        ::log::info!("");
        ::log::info!("📱 ESP BLE Provisioning QR Code:");
        ::log::info!("Payload: {}", qr_payload);
        ::log::info!("");

        // Generate QR code
        if let Ok(qr_code) = self.generate_ascii_qr(&qr_payload) {
            ::log::info!("┌────────────────────────────────────────┐");
            ::log::info!("│ Scan with ESP BLE Provisioning app:   │");
            ::log::info!("└────────────────────────────────────────┘");
            ::log::info!("");

            // Print QR code
            for line in qr_code.lines() {
                ::log::info!("{}", line);
            }

            ::log::info!("");
            ::log::info!("📱 Download: ESP BLE Provisioning (iOS/Android)");
            ::log::info!("🔗 iOS: https://apps.apple.com/app/esp-ble-provisioning/id1473590141");
            ::log::info!(
                "🔗 Android: https://play.google.com/store/apps/details?id=com.espressif.provble"
            );
        } else {
            ::log::warn!("⚠️ Failed to generate QR code - use manual connection:");
            ::log::info!("📱 Device: {}", device_name);
            if let Some(pop_str) = pop {
                ::log::info!("🔐 POP: {}", pop_str);
            }
        }

        ::log::info!("");
    }

    /// Generate ASCII QR code for terminal display using real QR library
    fn generate_ascii_qr(&self, data: &str) -> Result<String, ()> {
        use qrcode::QrCode;

        // Generate QR code
        let code = QrCode::new(data).map_err(|_| ())?;
        let image = code
            .render::<char>()
            .quiet_zone(false)
            .module_dimensions(2, 1)
            .dark_color('█')
            .light_color(' ')
            .build();

        Ok(image)
    }
}

impl Drop for WifiProvisioning {
    fn drop(&mut self) {
        if self.is_initialized {
            ::log::debug!("🧹 Cleaning up WiFi provisioning manager");
            unsafe {
                wifi_prov_mgr_deinit();
            }
        }
    }
}

/// WiFi Manager for station mode operation
pub struct WifiManager {
    // TODO: Add WiFi station management
}

impl WifiManager {
    pub fn new() -> Self {
        WifiManager {}
    }

    /// Connect to WiFi using stored credentials
    pub async fn connect_stored(&self) -> Result<(), EspError> {
        ::log::info!("📶 Attempting to connect to stored WiFi network");
        // TODO: Implement WiFi station connection
        Ok(())
    }

    /// Check if connected to WiFi
    pub fn is_connected(&self) -> bool {
        // TODO: Check WiFi connection status
        false
    }
}
