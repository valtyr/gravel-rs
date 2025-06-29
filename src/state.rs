use crate::types::{AutoTareState, BrewConfig, BrewState, ScaleData, SystemState, TimerState};
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, mutex::Mutex};
use embassy_time::Instant;
use log::{debug, info};
use std::sync::Arc;

pub struct StateManager {
    state: Arc<Mutex<CriticalSectionRawMutex, SystemState>>,
}

impl StateManager {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(SystemState::default())),
        }
    }

    pub fn get_state_handle(&self) -> Arc<Mutex<CriticalSectionRawMutex, SystemState>> {
        Arc::clone(&self.state)
    }

    pub async fn update_scale_data(&self, scale_data: ScaleData) {
        let mut state = self.state.lock().await;
        let weight = scale_data.weight_g;
        let flow_rate = scale_data.flow_rate_g_per_s;
        state.scale_data = Some(scale_data);
        self.add_log_message(
            &mut state,
            format!("Scale: {:.2}g, {:.2}g/s", weight, flow_rate),
        );
    }

    pub async fn update_timer_state(&self, timer_state: TimerState) {
        let mut state = self.state.lock().await;
        if state.timer_state != timer_state {
            info!(
                "Timer state changed: {:?} -> {:?}",
                state.timer_state, timer_state
            );
            state.timer_state = timer_state;
            self.add_log_message(&mut state, format!("Timer: {:?}", timer_state));
        }
    }

    pub async fn update_brew_state(&self, brew_state: BrewState) {
        let mut state = self.state.lock().await;
        if state.brew_state != brew_state {
            info!(
                "Brew state changed: {:?} -> {:?}",
                state.brew_state, brew_state
            );
            state.brew_state = brew_state;
            self.add_log_message(&mut state, format!("Brew: {:?}", brew_state));
        }
    }

    pub async fn update_auto_tare_state(&self, auto_tare_state: AutoTareState) {
        let mut state = self.state.lock().await;
        if state.auto_tare_state != auto_tare_state {
            debug!(
                "Auto-tare state changed: {:?} -> {:?}",
                state.auto_tare_state, auto_tare_state
            );
            state.auto_tare_state = auto_tare_state;
        }
    }

    pub async fn update_config(&self, config: BrewConfig) {
        let mut state = self.state.lock().await;
        state.config = config;
        self.add_log_message(&mut state, "Configuration updated".to_string());
    }

    pub async fn set_relay_enabled(&self, enabled: bool) {
        let mut state = self.state.lock().await;
        if state.relay_enabled != enabled {
            info!(
                "Relay state changed: {}",
                if enabled { "ON" } else { "OFF" }
            );
            state.relay_enabled = enabled;
            self.add_log_message(
                &mut state,
                format!("Relay: {}", if enabled { "ON" } else { "OFF" }),
            );
        }
    }

    pub async fn set_ble_connected(&self, connected: bool) {
        let mut state = self.state.lock().await;
        if state.ble_connected != connected {
            info!(
                "BLE connection changed: {}",
                if connected {
                    "Connected"
                } else {
                    "Disconnected"
                }
            );
            state.ble_connected = connected;
            self.add_log_message(
                &mut state,
                format!(
                    "BLE: {}",
                    if connected {
                        "Connected"
                    } else {
                        "Disconnected"
                    }
                ),
            );
        }
    }

    pub async fn set_wifi_connected(&self, connected: bool) {
        let mut state = self.state.lock().await;
        if state.wifi_connected != connected {
            info!(
                "Wi-Fi connection changed: {}",
                if connected {
                    "Connected"
                } else {
                    "Disconnected"
                }
            );
            state.wifi_connected = connected;
            self.add_log_message(
                &mut state,
                format!(
                    "Wi-Fi: {}",
                    if connected {
                        "Connected"
                    } else {
                        "Disconnected"
                    }
                ),
            );
        }
    }

    pub async fn set_error(&self, error: Option<String>) {
        let mut state = self.state.lock().await;
        state.last_error = error.clone();
        if let Some(err) = error {
            self.add_log_message(&mut state, format!("ERROR: {}", err));
        }
    }

    pub async fn add_log(&self, message: String) {
        let mut state = self.state.lock().await;
        self.add_log_message(&mut state, message);
    }

    fn add_log_message(&self, state: &mut SystemState, message: String) {
        // Use a simple counter instead of timestamp for embedded compatibility
        static mut COUNTER: u32 = 0;
        let count = unsafe {
            COUNTER += 1;
            COUNTER
        };
        let log_entry = format!("[{}] {}", count, message);

        if state.log_messages.len() >= 100 {
            state.log_messages.remove(0);
        }

        let _ = state.log_messages.push(log_entry);
    }

    pub async fn get_current_weight(&self) -> Option<f32> {
        let state = self.state.lock().await;
        state.scale_data.as_ref().map(|d| d.weight_g)
    }

    pub async fn get_target_weight(&self) -> f32 {
        let state = self.state.lock().await;
        state.config.target_weight_g
    }

    pub async fn get_current_flow_rate(&self) -> Option<f32> {
        let state = self.state.lock().await;
        state.scale_data.as_ref().map(|d| d.flow_rate_g_per_s)
    }

    pub async fn is_auto_tare_enabled(&self) -> bool {
        let state = self.state.lock().await;
        state.config.auto_tare
    }

    pub async fn is_predictive_stop_enabled(&self) -> bool {
        let state = self.state.lock().await;
        state.config.predictive_stop
    }

    pub async fn get_timer_state(&self) -> TimerState {
        let state = self.state.lock().await;
        state.timer_state
    }

    pub async fn get_brew_state(&self) -> BrewState {
        let state = self.state.lock().await;
        state.brew_state
    }

    pub async fn get_auto_tare_state(&self) -> AutoTareState {
        let state = self.state.lock().await;
        state.auto_tare_state
    }

    pub async fn is_ble_connected(&self) -> bool {
        let state = self.state.lock().await;
        state.ble_connected
    }

    pub async fn is_wifi_connected(&self) -> bool {
        let state = self.state.lock().await;
        state.wifi_connected
    }

    pub async fn is_relay_enabled(&self) -> bool {
        let state = self.state.lock().await;
        state.relay_enabled
    }

    pub async fn get_config(&self) -> BrewConfig {
        let state = self.state.lock().await;
        state.config.clone()
    }

    pub async fn get_full_state(&self) -> SystemState {
        let state = self.state.lock().await;
        state.clone()
    }

    pub async fn reset_to_idle(&self) {
        let mut state = self.state.lock().await;
        state.timer_state = TimerState::Idle;
        state.brew_state = BrewState::Idle;
        state.relay_enabled = false;
        state.last_error = None;
        self.add_log_message(&mut state, "System reset to idle state".to_string());
    }
}
