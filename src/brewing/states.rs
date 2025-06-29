//! Enhanced brewing state machine with killswitch functionality
//! States: SystemDisabled, ScaleDisconnected, Idle, Brewing, Settling

use crate::system::events::UserEvent;
use crate::types::{AutoTareState, ScaleData, TARE_COOLDOWN_MS, TARE_STABILITY_THRESHOLD_G, OVERSHOOT_HISTORY_SIZE};
use embassy_time::{Duration, Instant};
use heapless::Vec;
use log::{debug, info};
use statig::prelude::*;

// Overshoot measurement for learning
#[derive(Debug, Clone)]
struct OvershootMeasurement {
    overshoot: f32,
    timestamp: Instant,
}

// Input events to the state machine
#[derive(Debug, Clone)]
pub enum BrewInput {
    // System control
    EnableSystem,   // Turn off killswitch
    DisableSystem,  // Turn on killswitch - ignore all scale input
    
    // Network connectivity events
    BleEnabled,
    BleDisabled,
    BleScanning,
    BleConnecting,
    WifiConnected,
    WifiDisconnected,
    WifiConnecting,
    
    // From scale (ignored when system disabled)
    ScaleData(ScaleData),
    ScaleConnected,
    ScaleDisconnected,

    // From user (some work even when disabled)
    UserCommand(UserEvent),

    // From system
    TargetWeightReached { weight: f32, target: f32 },
    FlowStopped,
    SettlingTimeout,
    EmergencyStop,
    
    // Auto-tare events
    AutoTareTriggered,
    AutoTareEnabled,
    AutoTareDisabled,
    
    // Overshoot control events
    PredictiveStopTriggered { predicted_final_weight: f32 },
    OvershootRecorded { overshoot: f32 },
    OvershootReset,
    
    // Time-based events
    Tick,
    DelayedStopTimeout,
}

// Output events from the state machine
#[derive(Debug, Clone)]
pub enum BrewOutput {
    // To scale
    StartTimer,
    StopTimer,
    TareScale,

    // To relay
    RelayOn,
    RelayOff,

    // Network connectivity outputs
    EnableBle,
    DisableBle,
    StartBleScanning,
    StopBleScanning,
    ConnectToWifi { ssid: String, password: String },
    DisconnectWifi,
    StartWifiProvisioning,

    // To UI/system
    StateChanged { from: SystemState, to: SystemState },
    SystemEnabled,
    SystemDisabled,
    ScaleConnectionChanged { connected: bool },
    NetworkStatusChanged { ble_enabled: bool, wifi_connected: bool },
    PredictiveStopTriggered,
    BrewingStarted,
    BrewingFinished,
    DisplayUpdate,
    
    // Auto-tare outputs
    AutoTareStateChanged { from: AutoTareState, to: AutoTareState },
    AutoTareExecuted,
    
    // Overshoot control outputs
    PredictiveStopScheduled { delay_ms: i32, predicted_weight: f32 },
    OvershootLearningUpdated { delay_ms: i32, ewma: f32, confidence: f32 },
    OvershootControllerReset,
}

// System-level states - flat but with logical grouping  
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SystemState {
    // 🚫 Killswitch engaged - ignore all scale input
    SystemDisabled,
    
    // 🔌 Network connectivity states
    BleDisabled,        // BLE not enabled
    BleEnabled,         // BLE enabled but not scanning
    BleScanning,        // BLE scanning for devices
    BleConnecting,      // BLE connecting to scale
    
    // 📡 WiFi states  
    WifiDisconnected,   // WiFi not connected
    WifiConnecting,     // WiFi connecting
    WifiConnected,      // WiFi connected but scale disconnected
    
    // 📱 Scale connection states (requires BLE)
    ScaleDisconnected,  // BLE connected but scale not found/connected
    
    // ☕ Brewing states (scale connected)
    Idle,              // Scale connected, ready to brew
    Brewing,           // Active brewing in progress
    Settling,          // Post-brew settling period
}

// Legacy compatibility
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BrewState {
    Idle,
    Brewing,
    Settling,
}

// Shared context for the state machine
#[derive(Debug)]
pub struct BrewContext {
    settle_start_time: Option<Instant>,
    last_weight: Option<f32>,
    current_weight: f32,
    target_weight: f32,
    settling_timeout: Duration,
    timer_running: bool,
    
    // Network connectivity state
    ble_enabled: bool,
    ble_scanning: bool,
    wifi_connected: bool,
    wifi_connecting: bool,
    scale_connected: bool,
    
    // Auto-tare state
    auto_tare_enabled: bool,
    auto_tare_state: AutoTareState,
    auto_tare_stable_weight: f32,
    auto_tare_weight_history: Vec<f32, 10>,
    auto_tare_last_tare_time: Option<Instant>,
    auto_tare_brewing_cooldown_time: Option<Instant>,
    auto_tare_empty_threshold: f32,
    auto_tare_stable_readings_needed: usize,
    
    // Overshoot control state
    overshoot_stop_delay_ms: i32,
    overshoot_history: Vec<OvershootMeasurement, OVERSHOOT_HISTORY_SIZE>,
    overshoot_pending_predicted_stop: bool,
    overshoot_ewma: f32,                           // Exponentially weighted moving average
    overshoot_learning_rate: f32,                  // Adaptive learning rate (0.1 to 0.5)
    overshoot_confidence_score: f32,               // Learning confidence (0.0 to 1.0)
    overshoot_brew_count: u32,                     // Total brews for confidence calculation
    overshoot_pending_stop_time: Option<Instant>,  // Scheduled delayed stop time
    
    // System state
    system_enabled: bool,
    outputs: heapless::Vec<BrewOutput, 10>, // Collect outputs during state transitions
}

impl Default for BrewContext {
    fn default() -> Self {
        Self {
            settle_start_time: None,
            last_weight: None,
            current_weight: 0.0,
            target_weight: 36.0,
            settling_timeout: Duration::from_secs(5),
            timer_running: false,
            
            // Network connectivity defaults
            ble_enabled: false,      // Start with BLE disabled
            ble_scanning: false,
            wifi_connected: false,
            wifi_connecting: false,
            scale_connected: false,
            
            // Auto-tare defaults
            auto_tare_enabled: true,                        // Default enabled like Python
            auto_tare_state: AutoTareState::Empty,          // Start empty
            auto_tare_stable_weight: 0.0,
            auto_tare_weight_history: Vec::new(),
            auto_tare_last_tare_time: None,
            auto_tare_brewing_cooldown_time: None,
            auto_tare_empty_threshold: 2.0,                 // From Python
            auto_tare_stable_readings_needed: 5,            // From Python
            
            // Overshoot control defaults
            overshoot_stop_delay_ms: 500,                   // Initial delay from Python
            overshoot_history: Vec::new(),
            overshoot_pending_predicted_stop: false,
            overshoot_ewma: 0.0,                            // Exponentially weighted moving average
            overshoot_learning_rate: 0.3,                   // 30% new data, 70% historical
            overshoot_confidence_score: 0.0,                // Learning confidence
            overshoot_brew_count: 0,                        // Total brews for confidence calculation
            overshoot_pending_stop_time: None,              // No scheduled stop initially
            
            // System defaults
            system_enabled: true,    // Start enabled
            outputs: heapless::Vec::new(),
        }
    }
}

// Enhanced state machine with killswitch functionality
#[derive(Debug, Default)]
pub struct BrewStateMachine;

#[state_machine(
    initial = "State::ble_disabled()",
    state(derive(Debug)),
    on_transition = "Self::on_transition"
)]
impl BrewStateMachine {
    /// 🚫 KILLSWITCH STATE - System disabled, ignore all scale input
    #[state]
    fn system_disabled(context: &mut BrewContext, event: &BrewInput) -> Response<State> {
        use Response::*;
        
        match event {
            BrewInput::EnableSystem => {
                context.system_enabled = true;
                context.outputs.push(BrewOutput::SystemEnabled);
                // Transition based on current network state
                if context.ble_enabled {
                    if context.scale_connected {
                        Transition(State::idle())
                    } else {
                        Transition(State::scale_disconnected())
                    }
                } else {
                    Transition(State::ble_disabled())
                }
            }
            BrewInput::UserCommand(UserEvent::EmergencyStop) => {
                // Emergency stop works even when disabled
                context.outputs.push(BrewOutput::RelayOff);
                Handled
            }
            // All other events ignored when system disabled
            _ => Handled,
        }
    }

    /// 🔌 BLE DISABLED STATE - BLE not enabled
    #[state]
    fn ble_disabled(context: &mut BrewContext, event: &BrewInput) -> Response<State> {
        use Response::*;
        
        match event {
            BrewInput::DisableSystem => {
                context.system_enabled = false;
                context.outputs.push(BrewOutput::SystemDisabled);
                Transition(State::system_disabled())
            }
            BrewInput::BleEnabled => {
                context.ble_enabled = true;
                context.outputs.push(BrewOutput::EnableBle);
                context.outputs.push(BrewOutput::NetworkStatusChanged { 
                    ble_enabled: true, 
                    wifi_connected: context.wifi_connected 
                });
                Transition(State::ble_enabled())
            }
            BrewInput::EmergencyStop => {
                context.outputs.push(BrewOutput::RelayOff);
                Handled
            }
            // Ignore scale/wifi events when BLE disabled
            _ => Handled,
        }
    }

    /// 🔌 BLE ENABLED STATE - BLE enabled but not scanning
    #[state]
    fn ble_enabled(context: &mut BrewContext, event: &BrewInput) -> Response<State> {
        use Response::*;
        
        match event {
            BrewInput::DisableSystem => {
                context.system_enabled = false;
                context.outputs.push(BrewOutput::SystemDisabled);
                Transition(State::system_disabled())
            }
            BrewInput::BleDisabled => {
                context.ble_enabled = false;
                context.outputs.push(BrewOutput::DisableBle);
                context.outputs.push(BrewOutput::NetworkStatusChanged { 
                    ble_enabled: false, 
                    wifi_connected: context.wifi_connected 
                });
                Transition(State::ble_disabled())
            }
            BrewInput::BleScanning => {
                context.ble_scanning = true;
                context.outputs.push(BrewOutput::StartBleScanning);
                Transition(State::ble_scanning())
            }
            BrewInput::WifiConnected => {
                context.wifi_connected = true;
                context.outputs.push(BrewOutput::NetworkStatusChanged { 
                    ble_enabled: true, 
                    wifi_connected: true 
                });
                Handled
            }
            BrewInput::WifiDisconnected => {
                context.wifi_connected = false;
                context.outputs.push(BrewOutput::NetworkStatusChanged { 
                    ble_enabled: true, 
                    wifi_connected: false 
                });
                Handled
            }
            BrewInput::EmergencyStop => {
                context.outputs.push(BrewOutput::RelayOff);
                Handled
            }
            _ => Handled,
        }
    }

    /// 🔍 BLE SCANNING STATE - BLE scanning for devices
    #[state]
    fn ble_scanning(context: &mut BrewContext, event: &BrewInput) -> Response<State> {
        use Response::*;
        
        match event {
            BrewInput::DisableSystem => {
                context.system_enabled = false;
                context.outputs.push(BrewOutput::SystemDisabled);
                Transition(State::system_disabled())
            }
            BrewInput::BleDisabled => {
                context.ble_enabled = false;
                context.ble_scanning = false;
                context.outputs.push(BrewOutput::StopBleScanning);
                context.outputs.push(BrewOutput::DisableBle);
                context.outputs.push(BrewOutput::NetworkStatusChanged { 
                    ble_enabled: false, 
                    wifi_connected: context.wifi_connected 
                });
                Transition(State::ble_disabled())
            }
            BrewInput::BleConnecting => {
                context.ble_scanning = false;
                context.outputs.push(BrewOutput::StopBleScanning);
                Transition(State::ble_connecting())
            }
            BrewInput::ScaleConnected => {
                context.ble_scanning = false;
                context.scale_connected = true;
                context.outputs.push(BrewOutput::StopBleScanning);
                context.outputs.push(BrewOutput::ScaleConnectionChanged { connected: true });
                Transition(State::idle())
            }
            BrewInput::WifiConnected => {
                context.wifi_connected = true;
                context.outputs.push(BrewOutput::NetworkStatusChanged { 
                    ble_enabled: true, 
                    wifi_connected: true 
                });
                Handled
            }
            BrewInput::WifiDisconnected => {
                context.wifi_connected = false;
                context.outputs.push(BrewOutput::NetworkStatusChanged { 
                    ble_enabled: true, 
                    wifi_connected: false 
                });
                Handled
            }
            BrewInput::EmergencyStop => {
                context.outputs.push(BrewOutput::RelayOff);
                Handled
            }
            _ => Handled,
        }
    }

    /// 🔗 BLE CONNECTING STATE - BLE connecting to scale
    #[state]
    fn ble_connecting(context: &mut BrewContext, event: &BrewInput) -> Response<State> {
        use Response::*;
        
        match event {
            BrewInput::DisableSystem => {
                context.system_enabled = false;
                context.outputs.push(BrewOutput::SystemDisabled);
                Transition(State::system_disabled())
            }
            BrewInput::BleDisabled => {
                context.ble_enabled = false;
                context.ble_scanning = false;
                context.outputs.push(BrewOutput::DisableBle);
                context.outputs.push(BrewOutput::NetworkStatusChanged { 
                    ble_enabled: false, 
                    wifi_connected: context.wifi_connected 
                });
                Transition(State::ble_disabled())
            }
            BrewInput::ScaleConnected => {
                context.scale_connected = true;
                context.outputs.push(BrewOutput::ScaleConnectionChanged { connected: true });
                Transition(State::idle())
            }
            BrewInput::ScaleDisconnected => {
                // Connection failed, go back to scanning
                Transition(State::ble_scanning())
            }
            BrewInput::WifiConnected => {
                context.wifi_connected = true;
                context.outputs.push(BrewOutput::NetworkStatusChanged { 
                    ble_enabled: true, 
                    wifi_connected: true 
                });
                Handled
            }
            BrewInput::WifiDisconnected => {
                context.wifi_connected = false;
                context.outputs.push(BrewOutput::NetworkStatusChanged { 
                    ble_enabled: true, 
                    wifi_connected: false 
                });
                Handled
            }
            BrewInput::EmergencyStop => {
                context.outputs.push(BrewOutput::RelayOff);
                Handled
            }
            _ => Handled,
        }
    }

    /// 📱 SCALE DISCONNECTED STATE
    #[state]
    fn scale_disconnected(context: &mut BrewContext, event: &BrewInput) -> Response<State> {
        use Response::*;
        
        match event {
            BrewInput::DisableSystem => {
                context.system_enabled = false;
                context.outputs.push(BrewOutput::SystemDisabled);
                Transition(State::system_disabled())
            }
            BrewInput::BleDisabled => {
                context.ble_enabled = false;
                context.outputs.push(BrewOutput::DisableBle);
                context.outputs.push(BrewOutput::NetworkStatusChanged { 
                    ble_enabled: false, 
                    wifi_connected: context.wifi_connected 
                });
                Transition(State::ble_disabled())
            }
            BrewInput::BleScanning => {
                context.ble_scanning = true;
                context.outputs.push(BrewOutput::StartBleScanning);
                Transition(State::ble_scanning())
            }
            BrewInput::WifiConnected => {
                context.wifi_connected = true;
                context.outputs.push(BrewOutput::NetworkStatusChanged { 
                    ble_enabled: context.ble_enabled, 
                    wifi_connected: true 
                });
                Handled
            }
            BrewInput::WifiDisconnected => {
                context.wifi_connected = false;
                context.outputs.push(BrewOutput::NetworkStatusChanged { 
                    ble_enabled: context.ble_enabled, 
                    wifi_connected: false 
                });
                Handled
            }
            BrewInput::EmergencyStop => {
                context.outputs.push(BrewOutput::RelayOff);
                Handled
            }
            BrewInput::ScaleConnected => {
                context.scale_connected = true;
                context.outputs.push(BrewOutput::ScaleConnectionChanged { connected: true });
                Transition(State::idle())
            }
            BrewInput::UserCommand(UserEvent::TareScale) |
            BrewInput::UserCommand(UserEvent::StartBrewing) |
            BrewInput::UserCommand(UserEvent::StopBrewing) => {
                info!("Scale command ignored - scale not connected");
                Handled
            }
            // Ignore scale data when disconnected
            BrewInput::ScaleData(_) => Handled,
            _ => Handled,
        }
    }

    /// ⏸️ IDLE STATE - Ready to brew (scale connected)
    #[state]
    fn idle(context: &mut BrewContext, event: &BrewInput) -> Response<State> {
        use Response::*;

        match event {
            BrewInput::DisableSystem => {
                context.system_enabled = false;
                context.outputs.push(BrewOutput::SystemDisabled);
                Transition(State::system_disabled())
            }
            BrewInput::WifiConnected => {
                context.wifi_connected = true;
                context.outputs.push(BrewOutput::NetworkStatusChanged { 
                    ble_enabled: context.ble_enabled, 
                    wifi_connected: true 
                });
                Handled
            }
            BrewInput::WifiDisconnected => {
                context.wifi_connected = false;
                context.outputs.push(BrewOutput::NetworkStatusChanged { 
                    ble_enabled: context.ble_enabled, 
                    wifi_connected: false 
                });
                Handled
            }
            BrewInput::EmergencyStop => {
                context.outputs.push(BrewOutput::RelayOff);
                Handled
            }
            BrewInput::ScaleDisconnected => {
                context.scale_connected = false;
                context.outputs.push(BrewOutput::ScaleConnectionChanged { connected: false });
                Transition(State::scale_disconnected())
            }
            BrewInput::ScaleData(data) => {
                // Update context with scale data
                context.current_weight = data.weight_g;
                context.timer_running = data.timer_running;
                context.outputs.push(BrewOutput::DisplayUpdate);
                
                // Check auto-tare logic (only in idle state when not brewing)
                if Self::should_auto_tare(context, data.weight_g) {
                    Self::record_auto_tare(context);
                    context.outputs.push(BrewOutput::AutoTareExecuted);
                    context.outputs.push(BrewOutput::TareScale);
                }
                
                // Check if timer just started
                if data.timer_running && !context.timer_running {
                    context.timer_running = true;
                    context.last_weight = Some(data.weight_g);
                    context.outputs.push(BrewOutput::RelayOn);
                    context.outputs.push(BrewOutput::BrewingStarted);
                    return Transition(State::brewing());
                }
                
                Handled
            }
            BrewInput::UserCommand(UserEvent::StartBrewing) => {
                context.outputs.push(BrewOutput::StartTimer);
                Handled
            }
            BrewInput::UserCommand(UserEvent::TareScale) => {
                context.outputs.push(BrewOutput::TareScale);
                Handled
            }
            BrewInput::AutoTareEnabled => {
                context.auto_tare_enabled = true;
                Handled
            }
            BrewInput::AutoTareDisabled => {
                context.auto_tare_enabled = false;
                Handled
            }
            BrewInput::OvershootReset => {
                Self::reset_overshoot_controller(context);
                Handled
            }
            _ => Handled,
        }
    }

    /// ☕ BREWING STATE - Active brewing in progress
    #[state]
    fn brewing(context: &mut BrewContext, event: &BrewInput) -> Response<State> {
        use Response::*;

        match event {
            BrewInput::DisableSystem => {
                context.system_enabled = false;
                context.outputs.push(BrewOutput::SystemDisabled);
                context.outputs.push(BrewOutput::RelayOff);
                Transition(State::system_disabled())
            }
            BrewInput::WifiConnected => {
                context.wifi_connected = true;
                context.outputs.push(BrewOutput::NetworkStatusChanged { 
                    ble_enabled: context.ble_enabled, 
                    wifi_connected: true 
                });
                Handled
            }
            BrewInput::WifiDisconnected => {
                context.wifi_connected = false;
                context.outputs.push(BrewOutput::NetworkStatusChanged { 
                    ble_enabled: context.ble_enabled, 
                    wifi_connected: false 
                });
                Handled
            }
            BrewInput::EmergencyStop => {
                context.outputs.push(BrewOutput::RelayOff);
                if context.scale_connected {
                    Transition(State::idle())
                } else {
                    Transition(State::scale_disconnected())
                }
            }
            BrewInput::ScaleDisconnected => {
                context.scale_connected = false;
                context.outputs.push(BrewOutput::ScaleConnectionChanged { connected: false });
                context.outputs.push(BrewOutput::RelayOff);
                Transition(State::scale_disconnected())
            }
            BrewInput::ScaleData(data) => {
                context.current_weight = data.weight_g;
                context.last_weight = Some(data.weight_g);
                context.timer_running = data.timer_running;
                context.outputs.push(BrewOutput::DisplayUpdate);
                
                // Record overshoot when flow stops after predicted stop
                if data.flow_rate_g_per_s.abs() < 0.5 && context.overshoot_pending_predicted_stop {
                    let overshoot = data.weight_g - context.target_weight;
                    Self::record_overshoot_learning(context, overshoot);
                }
                
                // Check for predictive stop opportunity
                if let Some(predicted_weight) = Self::should_trigger_predictive_stop(context, data, context.target_weight) {
                    context.overshoot_pending_predicted_stop = true;
                    let time_to_target = (context.target_weight - data.weight_g) / data.flow_rate_g_per_s;
                    Self::schedule_delayed_stop(context, time_to_target);
                    context.outputs.push(BrewOutput::PredictiveStopTriggered);
                }
                
                // Check if delayed stop timeout occurred
                if Self::check_delayed_stop_timeout(context) {
                    context.overshoot_pending_stop_time = None;
                    context.outputs.push(BrewOutput::StopTimer);
                    context.outputs.push(BrewOutput::RelayOff);
                    context.settle_start_time = Some(Instant::now());
                    return Transition(State::settling());
                }
                
                // Check if timer stopped (manual or automatic)
                if !data.timer_running {
                    context.timer_running = false;
                    context.outputs.push(BrewOutput::RelayOff);
                    context.settle_start_time = Some(Instant::now());
                    return Transition(State::settling());
                }

                // Check target weight reached
                if data.weight_g >= context.target_weight {
                    // Mark as predicted stop if we had a scheduled stop
                    if context.overshoot_pending_stop_time.is_some() {
                        context.overshoot_pending_predicted_stop = true;
                    }
                    context.overshoot_pending_stop_time = None;
                    context.outputs.push(BrewOutput::StopTimer);
                    context.outputs.push(BrewOutput::RelayOff);
                    context.settle_start_time = Some(Instant::now());
                    return Transition(State::settling());
                }

                Handled
            }
            BrewInput::TargetWeightReached { .. } => {
                context.outputs.push(BrewOutput::StopTimer);
                context.outputs.push(BrewOutput::RelayOff);
                context.settle_start_time = Some(Instant::now());
                Transition(State::settling())
            }
            BrewInput::UserCommand(UserEvent::StopBrewing) => {
                context.outputs.push(BrewOutput::StopTimer);
                context.outputs.push(BrewOutput::RelayOff);
                context.settle_start_time = Some(Instant::now());
                Transition(State::settling())
            }
            BrewInput::UserCommand(UserEvent::TareScale) => {
                context.outputs.push(BrewOutput::TareScale);
                Handled
            }
            _ => Handled,
        }
    }

    /// 🕐 SETTLING STATE - Post-brew settling period
    #[state]
    fn settling(context: &mut BrewContext, event: &BrewInput) -> Response<State> {
        use Response::*;

        match event {
            BrewInput::DisableSystem => {
                context.system_enabled = false;
                context.outputs.push(BrewOutput::SystemDisabled);
                context.outputs.push(BrewOutput::RelayOff);
                Transition(State::system_disabled())
            }
            BrewInput::WifiConnected => {
                context.wifi_connected = true;
                context.outputs.push(BrewOutput::NetworkStatusChanged { 
                    ble_enabled: context.ble_enabled, 
                    wifi_connected: true 
                });
                Handled
            }
            BrewInput::WifiDisconnected => {
                context.wifi_connected = false;
                context.outputs.push(BrewOutput::NetworkStatusChanged { 
                    ble_enabled: context.ble_enabled, 
                    wifi_connected: false 
                });
                Handled
            }
            BrewInput::EmergencyStop => {
                context.outputs.push(BrewOutput::RelayOff);
                if context.scale_connected {
                    Transition(State::idle())
                } else {
                    Transition(State::scale_disconnected())
                }
            }
            BrewInput::ScaleDisconnected => {
                context.scale_connected = false;
                context.outputs.push(BrewOutput::ScaleConnectionChanged { connected: false });
                Transition(State::scale_disconnected())
            }
            BrewInput::ScaleData(data) => {
                context.current_weight = data.weight_g;
                context.timer_running = data.timer_running;
                context.outputs.push(BrewOutput::DisplayUpdate);
                
                // Check if timer restarted (new brew)
                if data.timer_running && !context.timer_running {
                    context.timer_running = true;
                    context.last_weight = Some(data.weight_g);
                    context.outputs.push(BrewOutput::RelayOn);
                    context.outputs.push(BrewOutput::BrewingStarted);
                    return Transition(State::brewing());
                }
                
                Handled
            }
            BrewInput::FlowStopped | BrewInput::SettlingTimeout => {
                context.outputs.push(BrewOutput::BrewingFinished);
                // Notify auto-tare that brewing finished
                Self::auto_tare_brewing_finished(context, context.current_weight);
                Transition(State::idle())
            }
            BrewInput::UserCommand(UserEvent::StartBrewing) => {
                context.outputs.push(BrewOutput::StartTimer);
                context.outputs.push(BrewOutput::RelayOn);
                context.outputs.push(BrewOutput::BrewingStarted);
                Transition(State::brewing())
            }
            BrewInput::UserCommand(UserEvent::TareScale) => {
                context.outputs.push(BrewOutput::TareScale);
                Handled
            }
            _ => Handled,
        }
    }

    fn on_transition(&mut self, source: &State, target: &State) {
        let source_state = Self::state_to_system_state(source);
        let target_state = Self::state_to_system_state(target);

        if source_state != target_state {
            info!("🔄 System state transition: {:?} -> {:?}", source_state, target_state);
        }
    }
    
    /// Convert internal State to SystemState for external interface
    fn state_to_system_state(state: &State) -> SystemState {
        match state {
            State::SystemDisabled {} => SystemState::SystemDisabled,
            State::BleDisabled {} => SystemState::BleDisabled,
            State::BleEnabled {} => SystemState::BleEnabled,
            State::BleScanning {} => SystemState::BleScanning,
            State::BleConnecting {} => SystemState::BleConnecting,
            State::ScaleDisconnected {} => SystemState::ScaleDisconnected,
            State::Idle {} => SystemState::Idle,
            State::Brewing {} => SystemState::Brewing,
            State::Settling {} => SystemState::Settling,
        }
    }
}

// Auto-tare helper functions
impl BrewStateMachine {
    /// Check if auto-tare should trigger based on current weight
    fn should_auto_tare(context: &mut BrewContext, current_weight: f32) -> bool {
        if !context.auto_tare_enabled 
            || context.timer_running 
            || !matches!(context.system_enabled, true) {
            return false;
        }

        // Check brewing cooldown period (prevent auto-tare right after brewing)
        if let Some(brewing_cooldown) = context.auto_tare_brewing_cooldown_time {
            if Instant::now().duration_since(brewing_cooldown) < Duration::from_secs(10) {
                debug!("Auto-tare: Still in brewing cooldown period");
                return false;
            }
        }

        // Check regular tare cooldown period
        if let Some(last_tare) = context.auto_tare_last_tare_time {
            if Instant::now().duration_since(last_tare) < Duration::from_millis(TARE_COOLDOWN_MS) {
                return false;
            }
        }

        let is_stable = Self::is_weight_stable(context, current_weight);
        let is_empty = current_weight.abs() <= context.auto_tare_empty_threshold;

        // State machine logic from Python
        match context.auto_tare_state {
            AutoTareState::Empty => {
                if !is_empty && is_stable {
                    // Object placed on empty scale - TARE IMMEDIATELY
                    let old_state = context.auto_tare_state;
                    context.auto_tare_state = AutoTareState::StableObject;
                    context.auto_tare_stable_weight = current_weight;
                    context.outputs.push(BrewOutput::AutoTareStateChanged { 
                        from: old_state, 
                        to: AutoTareState::StableObject 
                    });
                    info!("AutoTare: Object detected: {:.1}g - TARING", current_weight);
                    return true;
                } else if !is_empty {
                    // Weight detected but not stable yet
                    let old_state = context.auto_tare_state;
                    context.auto_tare_state = AutoTareState::Loading;
                    context.outputs.push(BrewOutput::AutoTareStateChanged { 
                        from: old_state, 
                        to: AutoTareState::Loading 
                    });
                }
            }

            AutoTareState::Loading => {
                if is_stable {
                    if is_empty {
                        // Stabilized to empty
                        let old_state = context.auto_tare_state;
                        context.auto_tare_state = AutoTareState::Empty;
                        context.auto_tare_stable_weight = 0.0;
                        context.outputs.push(BrewOutput::AutoTareStateChanged { 
                            from: old_state, 
                            to: AutoTareState::Empty 
                        });
                    } else {
                        // Stabilized with object - TARE IMMEDIATELY
                        let old_state = context.auto_tare_state;
                        context.auto_tare_state = AutoTareState::StableObject;
                        context.auto_tare_stable_weight = current_weight;
                        context.outputs.push(BrewOutput::AutoTareStateChanged { 
                            from: old_state, 
                            to: AutoTareState::StableObject 
                        });
                        info!("AutoTare: Object stabilized: {:.1}g - TARING", current_weight);
                        return true;
                    }
                }
            }

            AutoTareState::StableObject => {
                if is_empty && is_stable {
                    // Object removed - NO TARE, just go to Empty
                    let old_state = context.auto_tare_state;
                    context.auto_tare_state = AutoTareState::Empty;
                    context.auto_tare_stable_weight = 0.0;
                    context.outputs.push(BrewOutput::AutoTareStateChanged { 
                        from: old_state, 
                        to: AutoTareState::Empty 
                    });
                    info!("AutoTare: Object removed");
                } else if is_stable && (current_weight - context.auto_tare_stable_weight).abs() > 10.0 {
                    // MAJOR weight change - definitely cup swap (increased threshold to 10.0g for real-world use)
                    // Reset to Empty to force proper detection (NO IMMEDIATE TARE)
                    let old_state = context.auto_tare_state;
                    context.auto_tare_state = AutoTareState::Empty;
                    context.auto_tare_stable_weight = 0.0;
                    context.outputs.push(BrewOutput::AutoTareStateChanged { 
                        from: old_state, 
                        to: AutoTareState::Empty 
                    });
                    info!(
                        "AutoTare: Major cup change detected: {:.1}g -> {:.1}g",
                        context.auto_tare_stable_weight, current_weight
                    );
                } else if !is_stable {
                    // Weight changing - but only go to unloading if it's a significant change
                    // Small fluctuations after brewing shouldn't trigger unloading state
                    let recent_avg = if context.auto_tare_weight_history.len() >= 3 {
                        let recent: f32 = context.auto_tare_weight_history[context.auto_tare_weight_history.len() - 3..]
                            .iter()
                            .sum::<f32>()
                            / 3.0;
                        recent
                    } else {
                        current_weight
                    };

                    if (recent_avg - context.auto_tare_stable_weight).abs() > 5.0 {
                        let old_state = context.auto_tare_state;
                        context.auto_tare_state = AutoTareState::Unloading;
                        context.outputs.push(BrewOutput::AutoTareStateChanged { 
                            from: old_state, 
                            to: AutoTareState::Unloading 
                        });
                        info!("AutoTare: Major weight change detected, entering unloading state");
                    }
                    // Otherwise stay in StableObject state for small fluctuations
                }
            }

            AutoTareState::Unloading => {
                if is_stable {
                    if is_empty {
                        // Removed completely
                        let old_state = context.auto_tare_state;
                        context.auto_tare_state = AutoTareState::Empty;
                        context.auto_tare_stable_weight = 0.0;
                        context.outputs.push(BrewOutput::AutoTareStateChanged { 
                            from: old_state, 
                            to: AutoTareState::Empty 
                        });
                        info!("AutoTare: Object removed");
                    } else {
                        // Stabilized at new weight - TARE IMMEDIATELY
                        let old_state = context.auto_tare_state;
                        context.auto_tare_state = AutoTareState::StableObject;
                        let old_weight = context.auto_tare_stable_weight;
                        context.auto_tare_stable_weight = current_weight;
                        context.outputs.push(BrewOutput::AutoTareStateChanged { 
                            from: old_state, 
                            to: AutoTareState::StableObject 
                        });
                        info!(
                            "AutoTare: Object changed: {:.1}g → {:.1}g - TARING",
                            old_weight, current_weight
                        );
                        return true;
                    }
                }
            }
        }

        false
    }

    /// Check if weight is stable based on recent history
    fn is_weight_stable(context: &mut BrewContext, current_weight: f32) -> bool {
        // Add to history
        if context.auto_tare_weight_history.len() >= 10 {
            context.auto_tare_weight_history.remove(0);
        }
        let _ = context.auto_tare_weight_history.push(current_weight);

        // Need at least stable_readings_needed readings
        if context.auto_tare_weight_history.len() < context.auto_tare_stable_readings_needed {
            return false;
        }

        // Use Python's simple min/max approach for consistent behavior
        let recent_weights = &context.auto_tare_weight_history[context.auto_tare_weight_history.len() - context.auto_tare_stable_readings_needed..];

        // Check if recent weights are within threshold of each other (Python method)
        let max_weight = recent_weights
            .iter()
            .fold(f32::NEG_INFINITY, |a, &b| a.max(b));
        let min_weight = recent_weights.iter().fold(f32::INFINITY, |a, &b| a.min(b));

        // Consider stable if range is within threshold (exactly like Python)
        (max_weight - min_weight) <= TARE_STABILITY_THRESHOLD_G
    }

    /// Record that a tare was executed
    fn record_auto_tare(context: &mut BrewContext) {
        context.auto_tare_last_tare_time = Some(Instant::now());
    }

    /// Called when brewing finishes to preserve current object state
    fn auto_tare_brewing_finished(context: &mut BrewContext, current_weight: f32) {
        // Set brewing cooldown to prevent auto-tare for 10 seconds after brewing
        context.auto_tare_brewing_cooldown_time = Some(Instant::now());

        // If we have a stable object after brewing, keep it as stable without re-taring
        if current_weight > context.auto_tare_empty_threshold {
            let old_state = context.auto_tare_state;
            context.auto_tare_state = AutoTareState::StableObject;
            context.auto_tare_stable_weight = current_weight;
            context.outputs.push(BrewOutput::AutoTareStateChanged { 
                from: old_state, 
                to: AutoTareState::StableObject 
            });
            // Clear weight history to rebuild stability for this object
            context.auto_tare_weight_history.clear();
            info!(
                "AutoTare: Brewing finished, preserving object at {:.1}g (10s cooldown active)",
                current_weight
            );
        } else {
            let old_state = context.auto_tare_state;
            context.auto_tare_state = AutoTareState::Empty;
            context.auto_tare_stable_weight = 0.0;
            context.outputs.push(BrewOutput::AutoTareStateChanged { 
                from: old_state, 
                to: AutoTareState::Empty 
            });
            info!("AutoTare: Brewing finished, scale empty");
        }
    }
    
    /// Calculate valid prediction time window based on learned delay
    fn calculate_prediction_window(context: &BrewContext) -> (f32, f32) {
        let min_reaction_time = (context.overshoot_stop_delay_ms as f32 / 1000.0) + 0.2; // delay + safety margin
        let max_prediction_time = min_reaction_time * 3.0; // Don't predict too far ahead
        (min_reaction_time, max_prediction_time)
    }

    /// Get delay with overshoot compensation applied
    fn get_compensated_delay(context: &BrewContext, target_delay: f32) -> f32 {
        (target_delay - (context.overshoot_stop_delay_ms as f32 / 1000.0)).max(0.1)
    }

    /// Check if predictive stop should trigger based on current flow and weight
    fn should_trigger_predictive_stop(context: &BrewContext, scale_data: &ScaleData, target_weight: f32) -> Option<f32> {
        // Only in brewing state, with timer running and positive flow
        if scale_data.flow_rate_g_per_s <= 0.0 || scale_data.timestamp_ms <= 2000 {
            return None;
        }

        let weight_needed = target_weight - scale_data.weight_g;
        if weight_needed <= 0.0 {
            return None; // Already at or past target
        }

        let time_to_target = weight_needed / scale_data.flow_rate_g_per_s;
        let (min_time, max_time) = Self::calculate_prediction_window(context);

        debug!(
            "PREDICTION CHECK: weight={:.1}g, target={:.1}g, needed={:.1}g, flow={:.1}g/s, time_to_target={:.1}s, window=[{:.1}s, {:.1}s], delay={}ms",
            scale_data.weight_g, target_weight, weight_needed, scale_data.flow_rate_g_per_s,
            time_to_target, min_time, max_time, context.overshoot_stop_delay_ms
        );

        if min_time < time_to_target && time_to_target <= max_time {
            let predicted_final_weight = scale_data.weight_g + (scale_data.flow_rate_g_per_s * time_to_target);
            info!(
                "🎯 PREDICTIVE STOP TRIGGERED: time_to_target={:.1}s, predicted_weight={:.1}g",
                time_to_target, predicted_final_weight
            );
            Some(predicted_final_weight)
        } else {
            None
        }
    }

    /// Schedule a delayed stop with compensation
    fn schedule_delayed_stop(context: &mut BrewContext, delay_seconds: f32) {
        let compensated_delay = Self::get_compensated_delay(context, delay_seconds);
        let delay_duration = Duration::from_millis((compensated_delay * 1000.0) as u64);
        
        context.overshoot_pending_stop_time = Some(Instant::now() + delay_duration);
        context.outputs.push(BrewOutput::PredictiveStopScheduled { 
            delay_ms: (compensated_delay * 1000.0) as i32,
            predicted_weight: 0.0 // Will be filled in by caller
        });
        
        info!(
            "⏰ SCHEDULED STOP: in {:.1}s (compensated from {:.1}s)",
            compensated_delay, delay_seconds
        );
    }

    /// Record overshoot and update learning using EWMA algorithm
    fn record_overshoot_learning(context: &mut BrewContext, overshoot: f32) {
        if !context.overshoot_pending_predicted_stop {
            debug!("Overshoot: No pending predicted stop - skipping");
            return;
        }

        context.overshoot_pending_predicted_stop = false;
        
        // Add to history
        let measurement = OvershootMeasurement {
            overshoot,
            timestamp: Instant::now(),
        };
        if context.overshoot_history.len() >= OVERSHOOT_HISTORY_SIZE {
            context.overshoot_history.remove(0);
        }
        let _ = context.overshoot_history.push(measurement);

        // Update EWMA
        let old_ewma = context.overshoot_ewma;
        context.overshoot_ewma = context.overshoot_learning_rate * overshoot + 
                                 (1.0 - context.overshoot_learning_rate) * old_ewma;

        debug!(
            "EWMA update: {:.1}g + {:.1}g -> {:.1}g (rate={:.1}%)",
            old_ewma, overshoot, context.overshoot_ewma,
            context.overshoot_learning_rate * 100.0
        );

        // Update delay based on EWMA
        Self::update_overshoot_delay(context);

        // Update confidence and brew count
        context.overshoot_brew_count += 1;
        Self::update_overshoot_confidence(context);

        context.outputs.push(BrewOutput::OvershootLearningUpdated {
            delay_ms: context.overshoot_stop_delay_ms,
            ewma: context.overshoot_ewma,
            confidence: context.overshoot_confidence_score,
        });

        info!(
            "📊 Overshoot learning: {:.1}g -> ewma={:.1}g, delay={}ms, confidence={:.1}%, brews={}",
            overshoot, context.overshoot_ewma, context.overshoot_stop_delay_ms,
            context.overshoot_confidence_score * 100.0, context.overshoot_brew_count
        );
    }

    /// Update delay based on EWMA using proportional control
    fn update_overshoot_delay(context: &mut BrewContext) {
        let error_magnitude = context.overshoot_ewma.abs();
        let base_adjustment = (error_magnitude * 50.0).min(200.0).max(10.0); // 10-200ms range

        // Confidence modifier: less confident = smaller adjustments
        let confidence_modifier = (context.overshoot_confidence_score * 0.5 + 0.5).min(1.0);
        let adjustment = (base_adjustment * confidence_modifier) as i32;

        let old_delay = context.overshoot_stop_delay_ms;

        if context.overshoot_ewma > 0.5 {
            // Overshooting - stop earlier (increase delay)
            context.overshoot_stop_delay_ms += adjustment;
            context.overshoot_stop_delay_ms = context.overshoot_stop_delay_ms.min(2000); // Cap at 2 seconds
            info!(
                "🔼 Overshooting by {:.1}g, increasing delay by {}ms: {}ms -> {}ms",
                context.overshoot_ewma, adjustment, old_delay, context.overshoot_stop_delay_ms
            );
        } else if context.overshoot_ewma < -0.5 {
            // Undershooting - stop later (decrease delay)
            context.overshoot_stop_delay_ms -= adjustment;
            context.overshoot_stop_delay_ms = context.overshoot_stop_delay_ms.max(100); // Minimum 100ms
            info!(
                "🔽 Undershooting by {:.1}g, decreasing delay by {}ms: {}ms -> {}ms",
                context.overshoot_ewma.abs(), adjustment, old_delay, context.overshoot_stop_delay_ms
            );
        } else {
            debug!(
                "✅ EWMA within ±0.5g threshold, keeping delay at {}ms",
                context.overshoot_stop_delay_ms
            );
        }
    }

    /// Update learning confidence based on consistency
    fn update_overshoot_confidence(context: &mut BrewContext) {
        if context.overshoot_history.len() < 3 {
            context.overshoot_confidence_score = 0.0;
            return;
        }

        // Calculate consistency (lower variance = higher confidence)
        let mut overshoots = heapless::Vec::<f32, OVERSHOOT_HISTORY_SIZE>::new();
        for measurement in context.overshoot_history.iter() {
            let _ = overshoots.push(measurement.overshoot);
        }

        let mean: f32 = overshoots.iter().sum::<f32>() / overshoots.len() as f32;
        let variance: f32 = overshoots.iter()
            .map(|&x| (x - mean).powi(2))
            .sum::<f32>() / overshoots.len() as f32;

        let std_dev = variance.sqrt();

        // Convert consistency to confidence (lower std_dev = higher confidence)
        let consistency_score = (3.0f32 - std_dev).max(0.0) / 2.5; // 0.0 to 1.0

        // Experience factor: more brews = higher confidence
        let experience_factor = (context.overshoot_brew_count as f32 / 20.0).min(1.0);

        // Combined confidence
        context.overshoot_confidence_score = (consistency_score * experience_factor).min(1.0);

        // Update learning rate based on confidence
        context.overshoot_learning_rate = if context.overshoot_confidence_score > 0.8 {
            0.1 // Slow learning when confident
        } else if context.overshoot_confidence_score > 0.5 {
            0.2 // Medium learning
        } else {
            0.3 // Fast learning when uncertain
        };

        debug!(
            "Confidence update: consistency={:.2}, experience={:.2}, combined={:.2}, learning_rate={:.1}%",
            consistency_score, experience_factor, context.overshoot_confidence_score,
            context.overshoot_learning_rate * 100.0
        );
    }

    /// Reset overshoot controller to defaults
    fn reset_overshoot_controller(context: &mut BrewContext) {
        info!("🔄 Resetting overshoot controller to defaults");
        context.overshoot_history.clear();
        context.overshoot_stop_delay_ms = 500;
        context.overshoot_pending_predicted_stop = false;
        context.overshoot_ewma = 0.0;
        context.overshoot_confidence_score = 0.0;
        context.overshoot_brew_count = 0;
        context.overshoot_learning_rate = 0.3;
        context.overshoot_pending_stop_time = None;
        
        context.outputs.push(BrewOutput::OvershootControllerReset);
    }

    /// Check if delayed stop timeout has occurred
    fn check_delayed_stop_timeout(context: &BrewContext) -> bool {
        if let Some(stop_time) = context.overshoot_pending_stop_time {
            Instant::now() >= stop_time
        } else {
            false
        }
    }
}

// Main interface for the hierarchical state machine
pub struct BrewController {
    machine: statig::prelude::StateMachine<BrewStateMachine>,
    context: BrewContext,
}

impl BrewController {
    pub fn new() -> Self {
        Self {
            machine: BrewStateMachine::default().state_machine(),
            context: BrewContext::default(),
        }
    }

    /// Process an input event and return output events
    pub fn handle_input(&mut self, input: BrewInput) -> heapless::Vec<BrewOutput, 10> {
        // Clear previous outputs
        self.context.outputs.clear();

        // Capture current state before transition
        let previous_state = self.get_system_state();

        // Handle the input with context
        let _ = self.machine.handle_with_context(&input, &mut self.context);

        // Capture new state after transition
        let new_state = self.get_system_state();

        // Only emit StateChanged if the state actually changed
        if previous_state != new_state {
            self.context.outputs.push(BrewOutput::StateChanged {
                from: previous_state,
                to: new_state,
            });
        }

        // Return collected outputs
        std::mem::take(&mut self.context.outputs)
    }

    /// Get current system state (hierarchical)
    pub fn get_system_state(&self) -> SystemState {
        BrewStateMachine::state_to_system_state(self.machine.state())
    }

    /// Get current brewing state (legacy compatibility)
    pub fn get_state(&self) -> BrewState {
        match self.get_system_state() {
            SystemState::Idle => BrewState::Idle,
            SystemState::Brewing => BrewState::Brewing,
            SystemState::Settling => BrewState::Settling,
            _ => BrewState::Idle, // Default for non-brewing states
        }
    }

    /// Update target weight
    pub fn set_target_weight(&mut self, weight: f32) {
        self.context.target_weight = weight;
    }

    /// Get current context (for debugging/display)
    pub fn get_context(&self) -> &BrewContext {
        &self.context
    }

    /// Check for settling timeout (call periodically)
    pub fn check_settling_timeout(&mut self) -> heapless::Vec<BrewOutput, 10> {
        if let Some(settle_start) = self.context.settle_start_time {
            if Instant::now().duration_since(settle_start) > self.context.settling_timeout {
                return self.handle_input(BrewInput::SettlingTimeout);
            }
        }
        heapless::Vec::new()
    }

    /// Emergency stop (force to idle)
    pub fn emergency_stop(&mut self) -> heapless::Vec<BrewOutput, 10> {
        self.handle_input(BrewInput::EmergencyStop)
    }

    /// Enable/disable system (killswitch)
    pub fn set_system_enabled(&mut self, enabled: bool) -> heapless::Vec<BrewOutput, 10> {
        if enabled {
            self.handle_input(BrewInput::EnableSystem)
        } else {
            self.handle_input(BrewInput::DisableSystem)
        }
    }

    /// Check if system is enabled (not in killswitch mode)
    pub fn is_system_enabled(&self) -> bool {
        self.context.system_enabled
    }

    /// Enable/disable auto-tare
    pub fn set_auto_tare_enabled(&mut self, enabled: bool) -> heapless::Vec<BrewOutput, 10> {
        if enabled {
            self.handle_input(BrewInput::AutoTareEnabled)
        } else {
            self.handle_input(BrewInput::AutoTareDisabled)
        }
    }

    /// Check if auto-tare is enabled
    pub fn is_auto_tare_enabled(&self) -> bool {
        self.context.auto_tare_enabled
    }

    /// Get current auto-tare state
    pub fn get_auto_tare_state(&self) -> AutoTareState {
        self.context.auto_tare_state
    }

    /// Reset auto-tare state (useful for debugging or manual reset)
    pub fn reset_auto_tare(&mut self) {
        self.context.auto_tare_state = AutoTareState::Empty;
        self.context.auto_tare_weight_history.clear();
        self.context.auto_tare_stable_weight = 0.0;
    }

    /// Reset overshoot controller
    pub fn reset_overshoot(&mut self) -> heapless::Vec<BrewOutput, 10> {
        self.handle_input(BrewInput::OvershootReset)
    }

    /// Get current overshoot delay
    pub fn get_overshoot_delay_ms(&self) -> i32 {
        self.context.overshoot_stop_delay_ms
    }

    /// Get overshoot learning statistics
    pub fn get_overshoot_stats(&self) -> (f32, f32, u32) {
        (
            self.context.overshoot_ewma,
            self.context.overshoot_confidence_score,
            self.context.overshoot_brew_count,
        )
    }

    /// Check if overshoot learning is ready (has enough data)
    pub fn is_overshoot_learning_ready(&self) -> bool {
        self.context.overshoot_brew_count >= 3 && self.context.overshoot_confidence_score > 0.2
    }

    /// Get overshoot learning info as string for logging
    pub fn get_overshoot_learning_info(&self) -> String {
        format!(
            "Learning: delay={}ms, ewma={:.1}g, confidence={:.1}%, brews={}, ready={}",
            self.context.overshoot_stop_delay_ms,
            self.context.overshoot_ewma,
            self.context.overshoot_confidence_score * 100.0,
            self.context.overshoot_brew_count,
            self.is_overshoot_learning_ready()
        )
    }
}

// Convert between internal and external brew states for compatibility
impl From<BrewState> for crate::types::BrewState {
    fn from(state: BrewState) -> Self {
        match state {
            BrewState::Idle => crate::types::BrewState::Idle,
            BrewState::Brewing => crate::types::BrewState::Brewing,
            BrewState::Settling => crate::types::BrewState::BrewSettling,
        }
    }
}

impl From<crate::types::BrewState> for BrewState {
    fn from(state: crate::types::BrewState) -> Self {
        match state {
            crate::types::BrewState::Idle => BrewState::Idle,
            crate::types::BrewState::Brewing => BrewState::Brewing,
            crate::types::BrewState::BrewSettling => BrewState::Settling,
        }
    }
}

// Brewing state transition for compatibility with existing code
#[derive(Debug, Clone, Copy)]
pub struct BrewStateTransition {
    pub from: crate::types::BrewState,
    pub to: crate::types::BrewState,
}

impl BrewStateTransition {
    pub fn new(from: crate::types::BrewState, to: crate::types::BrewState) -> Self {
        Self { from, to }
    }
}