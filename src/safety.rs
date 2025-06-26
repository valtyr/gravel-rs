use crate::types::{SystemState, TimerState};
use embassy_time::{Duration, Instant};
use log::{error, warn, info};

pub struct SafetyController {
    last_data_received: Option<Instant>,
    last_relay_state: bool,
    watchdog_timeout: Duration,
}

impl SafetyController {
    pub fn new() -> Self {
        Self {
            last_data_received: None,
            last_relay_state: false,
            watchdog_timeout: Duration::from_secs(10),
        }
    }
    
    pub fn update_data_received(&mut self) {
        self.last_data_received = Some(Instant::now());
    }
    
    pub fn should_emergency_stop(&mut self, state: &SystemState) -> bool {
        let now = Instant::now();
        
        if state.timer_state == TimerState::Running {
            if !state.ble_connected {
                error!("SAFETY: BLE disconnected during brewing - emergency stop");
                return true;
            }
            
            if let Some(last_received) = self.last_data_received {
                if now.duration_since(last_received) > self.watchdog_timeout {
                    error!("SAFETY: Data watchdog timeout during brewing - emergency stop");
                    return true;
                }
            } else {
                error!("SAFETY: No data received during brewing - emergency stop");
                return true;
            }
            
            // TEMPORARY: Disable Wi-Fi safety check for BLE testing
            // if !state.wifi_connected {
            //     warn!("SAFETY: Wi-Fi disconnected during brewing - emergency stop for safety");
            //     return true;
            // }
            
            if state.last_error.is_some() {
                error!("SAFETY: System error during brewing - emergency stop");
                return true;
            }
        }
        
        false
    }
    
    pub fn handle_emergency_stop(&mut self, relay_controller: &mut crate::relay::RelayController) {
        if self.last_relay_state {
            error!("EMERGENCY STOP: Turning off relay immediately");
            if let Err(e) = relay_controller.turn_off_immediately() {
                error!("CRITICAL: Failed to turn off relay during emergency stop: {:?}", e);
            }
            self.last_relay_state = false;
        }
    }
    
    pub fn update_relay_state(&mut self, enabled: bool) {
        if enabled != self.last_relay_state {
            if enabled {
                info!("SAFETY: Relay turned ON");
            } else {
                info!("SAFETY: Relay turned OFF");
            }
        }
        self.last_relay_state = enabled;
    }
    
    pub fn check_system_health(&self, state: &SystemState) -> Vec<String> {
        let mut warnings = Vec::new();
        
        if !state.ble_connected {
            warnings.push("BLE not connected".to_string());
        }
        
        if !state.wifi_connected {
            warnings.push("Wi-Fi not connected".to_string());
        }
        
        if let Some(last_received) = self.last_data_received {
            let age = Instant::now().duration_since(last_received);
            if age > Duration::from_secs(5) {
                warnings.push(format!("No scale data for {}s", age.as_secs()));
            }
        } else {
            warnings.push("No scale data received".to_string());
        }
        
        if let Some(ref error) = state.last_error {
            warnings.push(format!("System error: {}", error));
        }
        
        if let Some(ref scale_data) = state.scale_data {
            if scale_data.battery_percent < 20 {
                warnings.push(format!("Low battery: {}%", scale_data.battery_percent));
            }
        }
        
        warnings
    }
}