use embassy_time::{Duration, Instant};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TimerState {
    Idle,
    Running,
    Paused,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BrewState {
    Idle,
    Brewing,
    BrewSettling,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AutoTareState {
    Empty,
    Loading,
    StableObject,
    Unloading,
}

#[derive(Debug, Clone)]
pub struct ScaleData {
    pub timestamp_ms: u32,
    pub weight_g: f32,
    pub flow_rate_g_per_s: f32,
    pub battery_percent: u8,
    pub timer_running: bool,
    pub received_at: Instant,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrewConfig {
    pub target_weight_g: f32,
    pub auto_tare: bool,
    pub predictive_stop: bool,
}

impl Default for BrewConfig {
    fn default() -> Self {
        Self {
            target_weight_g: 36.0,
            auto_tare: true,
            predictive_stop: true,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SystemState {
    pub scale_data: Option<ScaleData>,
    pub timer_state: TimerState,
    pub brew_state: BrewState,
    pub auto_tare_state: AutoTareState,
    pub config: BrewConfig,
    pub relay_enabled: bool,
    pub ble_connected: bool,
    pub wifi_connected: bool,
    pub last_error: Option<String>,
    pub log_messages: heapless::Vec<String, 100>,
}

impl Default for SystemState {
    fn default() -> Self {
        Self {
            scale_data: None,
            timer_state: TimerState::Idle,
            brew_state: BrewState::Idle,
            auto_tare_state: AutoTareState::Empty,
            config: BrewConfig::default(),
            relay_enabled: false,
            ble_connected: false,
            wifi_connected: false,
            last_error: None,
            log_messages: heapless::Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebSocketMessage {
    pub message_type: String,
    pub data: serde_json::Value,
}

pub const TARE_STABILITY_THRESHOLD_G: f32 = 0.5; // Match Python implementation for faster cup removal detection
pub const TARE_STABILITY_COUNT: usize = 5;
pub const TARE_COOLDOWN_MS: u64 = 2000;
pub const BREW_SETTLING_TIMEOUT_MS: u64 = 2000; // 2 seconds settling time
pub const OVERSHOOT_HISTORY_SIZE: usize = 5;
pub const PREDICTION_SAFETY_MARGIN_G: f32 = 2.0; // Increased from 0.5g to prevent early stops
