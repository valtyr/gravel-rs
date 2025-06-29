use crate::{
    ble::StatusChannel,
    brewing::{
        BrewController, BrewInput, BrewOutput, BrewStateTransition,
    },
    hardware::relay::{RelayController, RelayError},
    scales::{
        bookoo::BookooScale,
        event_detection::ScaleEventDetector,
        traits::{ScaleCommand, ScaleCommandChannel, ScaleDataChannel},
    },
    server::http::{WebSocketCommand, WebSocketCommandChannel, WebSocketServer},
    state::StateManager,
    system::{events::*, NvsStorage, SafetyController},
    types::{BrewConfig, BrewState, ScaleData, TimerState},
};
use embassy_executor::Spawner;
use embassy_futures::select::{select, Either};
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, channel::Channel};
use embassy_time::{Duration, Instant, Timer};
// BLE now handled by esp32-nimble crate
use esp_idf_svc::hal::gpio::Gpio19;
use log::{debug, error, info, warn};
use std::sync::Arc;

// Scale command channel type imported from traits

/// Comprehensive status for monitoring and debugging
#[derive(Debug)]
pub struct ComprehensiveStatus {
    pub system_state: crate::brewing::states::SystemState,
    pub scale_detector_timer_running: bool,
    pub scale_detector_stable_weight: Option<f32>,
    pub system_enabled: bool,
    pub state_manager_state: crate::types::SystemState,
}

pub struct EspressoController {
    state_manager: StateManager,
    scale_client: BookooScale,
    websocket_server: WebSocketServer,
    relay_controller: RelayController,
    safety_controller: SafetyController,
    brew_controller: BrewController,
    nvs_storage: Option<Arc<NvsStorage>>,

    // 🚀 WORLD-CLASS EVENT BUS!
    event_bus: Arc<EventBus>,

    // 🕵️ INTELLIGENT SCALE EVENT DETECTION!
    scale_event_detector: ScaleEventDetector,

    // Legacy channels (will be phased out)
    scale_data_channel: Arc<ScaleDataChannel>,
    ble_status_channel: Arc<StatusChannel>,
    websocket_command_channel: Arc<WebSocketCommandChannel>,
    scale_command_channel: Arc<ScaleCommandChannel>,

    // Predictive stopping state (Python style)
    pending_stop_time: Option<Instant>,

    // Timer detection state (from Python reference)
    last_timer_ms: Option<u32>,
    current_timer_running: bool,

    // Scale shutdown detection to prevent false brewing triggers
    timer_start_time: Option<Instant>,    // When timer was started
    consecutive_disconnection_count: u32, // Count BLE disconnections after timer start

    // Brewing startup delay to ignore button press artifacts
    brew_start_time: Option<Instant>,

}

impl EspressoController {
    pub async fn new(gpio19: Gpio19) -> Result<Self, Box<dyn std::error::Error>> {
        let scale_data_channel = Arc::new(Channel::new());
        let ble_status_channel = Arc::new(Channel::new());
        let websocket_command_channel = Arc::new(Channel::new());
        let scale_command_channel = Arc::new(Channel::new());

        let state_manager = StateManager::new();
        let state_handle = state_manager.get_state_handle();

        let scale_client = BookooScale::new(
            Arc::clone(&scale_data_channel),
            Arc::clone(&ble_status_channel),
        );

        let websocket_server = WebSocketServer::new(
            Arc::clone(&state_handle),
            Arc::clone(&websocket_command_channel),
            8080,
        );

        let relay_controller = RelayController::new(gpio19)?;

        // Initialize NVS storage (optional - will use defaults if it fails)
        let nvs_storage = match NvsStorage::new().await {
            Ok(storage) => {
                info!("✅ NVS storage initialized successfully");
                Some(Arc::new(storage))
            }
            Err(e) => {
                warn!(
                    "⚠️  NVS storage failed to initialize: {:?} - continuing with defaults",
                    e
                );
                None
            }
        };

        // Overshoot controller is now integrated into the state machine
        let mut brew_controller = BrewController::new();
        // Set initial target weight from default config
        brew_controller.set_target_weight(BrewConfig::default().target_weight_g);

        // 🚀 INITIALIZE WORLD-CLASS EVENT BUS!
        let event_bus = Arc::new(EventBus::new());

        info!("🌟 World-class EventBus initialized with type-safe subscriptions!");

        Ok(Self {
            state_manager,
            scale_client,
            websocket_server,
            relay_controller,
            safety_controller: SafetyController::new(),
            brew_controller,
            nvs_storage,

            // 🚀 WORLD-CLASS EVENT BUS!
            event_bus,

            // 🕵️ INTELLIGENT SCALE EVENT DETECTION!
            scale_event_detector: ScaleEventDetector::new(),

            // Legacy channels (being phased out)
            scale_data_channel,
            ble_status_channel,
            websocket_command_channel,
            scale_command_channel,

            // Predictive stopping
            pending_stop_time: None,

            // Timer detection state
            last_timer_ms: None,
            current_timer_running: false,

            // Scale shutdown detection
            timer_start_time: None,
            consecutive_disconnection_count: 0,

            // Brewing startup delay
            brew_start_time: None,

        })
    }

    pub async fn start(
        &mut self,
        spawner: Spawner,
        wifi_connected: bool,
        ble_needs_reset: bool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        info!("Starting Espresso Controller with Embassy tasks");

        // Handle BLE initialization based on WiFi provisioning status
        if ble_needs_reset {
            info!("🔄 BLE stack cleaned up by WiFi provisioning - reinitializing for scale");
            // WiFi provisioning already cleaned up BLE stack, just reinitialize
            BookooScale::initialize()
                .map_err(|e| format!("BLE init after provisioning failed: {:?}", e))?;
        } else if !wifi_connected {
            info!("🔵 No WiFi provisioning conflict - initializing scale BLE");
            BookooScale::initialize().map_err(|e| format!("BLE init failed: {:?}", e))?;
        } else {
            info!("🔵 WiFi connected without provisioning - initializing scale BLE");
            BookooScale::initialize().map_err(|e| format!("BLE init failed: {:?}", e))?;
        }

        // Clone references for the tasks
        let websocket_server = self.websocket_server.clone();
        let _state_handle = self.state_manager.get_state_handle();

        // Create a new scale client for the task (since tasks own their data)
        let scale_client = BookooScale::new(
            Arc::clone(&self.scale_data_channel),
            Arc::clone(&self.ble_status_channel),
        );

        // Spawn scale task with command channel
        spawner
            .spawn(scale_task(
                scale_client,
                Arc::clone(&self.scale_command_channel),
            ))
            .map_err(|_| "Failed to spawn scale task")?;

        // Spawn WebSocket/HTTP server task (non-fatal if it fails)
        if let Err(_) = spawner.spawn(websocket_task(websocket_server)) {
            warn!("Failed to spawn WebSocket task - continuing without HTTP server");
        }

        // Spawn scale data bridge task (CRITICAL - bridges scale data to event bus)
        spawner
            .spawn(scale_data_bridge_task(
                Arc::clone(&self.scale_data_channel),
                Arc::clone(&self.ble_status_channel),
                Arc::clone(&self.event_bus),
            ))
            .map_err(|_| "Failed to spawn scale data bridge task")?;

        // 🚀 Run the WORLD-CLASS event-driven control loop!
        self.event_driven_control_loop().await;

        Ok(())
    }

    async fn main_control_loop(&mut self) {
        info!("Starting main control loop with Embassy select");

        loop {
            let scale_data_fut = self.scale_data_channel.receive();
            let ble_status_fut = self.ble_status_channel.receive();
            let websocket_cmd_fut = self.websocket_command_channel.receive();
            let periodic_timer = Timer::after(Duration::from_millis(100));

            match select(
                select(scale_data_fut, ble_status_fut),
                select(websocket_cmd_fut, periodic_timer),
            )
            .await
            {
                Either::First(Either::First(scale_data)) => {
                    self.handle_scale_data(scale_data).await;
                }
                Either::First(Either::Second(ble_status)) => {
                    self.handle_ble_status_change(ble_status).await;
                }
                Either::Second(Either::First(command)) => {
                    self.handle_websocket_command(command).await;
                }
                Either::Second(Either::Second(_)) => {
                    // Periodic update
                    self.periodic_update().await;
                }
            }
        }
    }

    /// 🚀 PURE EVENT-DRIVEN CONTROL LOOP! NO LEGACY GARBAGE!
    /// Every action flows through events - total single source of truth!
    async fn event_driven_control_loop(&mut self) {
        info!("🔥 Starting PURE event-driven control loop - NO LEGACY!");

        // Clone event bus so we can create subscriber without borrowing self
        let event_bus = Arc::clone(&self.event_bus);
        let mut all_events_subscriber = event_bus.subscriber();

        // UNIFIED EVENT LOOP - process all events including hardware side effects!
        loop {
            let event_fut = all_events_subscriber.next_event();
            let periodic_timer = Timer::after(Duration::from_millis(100));

            match select(event_fut, periodic_timer).await {
                Either::First(event) => {
                    // Handle all event types including hardware side effects
                    match &event {
                        SystemEvent::Hardware(_) => {
                            self.handle_hardware_side_effects(event).await;
                        }
                        _ => {
                            self.handle_system_event(event).await;
                        }
                    }
                }
                Either::Second(_) => {
                    // Periodic tick
                    let event_publisher = event_bus.publisher();
                    event_publisher
                        .publish(SystemEvent::Time(TimeEvent::Tick))
                        .await;
                }
            }
        }
    }

    /// ⚡ PURE HARDWARE SIDE EFFECTS HANDLER - NO DIRECT HARDWARE CALLS ELSEWHERE!
    async fn handle_hardware_side_effects(&mut self, event: SystemEvent) {
        if let SystemEvent::Hardware(hardware_event) = event {
            match hardware_event {
                HardwareEvent::RelayOn => {
                    info!("⚡ HARDWARE: Relay ON");
                    if let Err(e) = self.relay_controller.turn_on().await {
                        error!("🚨 RELAY FAILED ON: {:?}", e);
                        self.get_event_publisher()
                            .emergency_stop("Relay failure".to_string())
                            .await;
                    } else {
                        self.state_manager.set_relay_enabled(true).await;
                    }
                }
                HardwareEvent::RelayOff => {
                    info!("⚡ HARDWARE: Relay OFF");
                    if let Err(e) = self.relay_controller.turn_off().await {
                        error!("🚨 RELAY FAILED OFF: {:?}", e);
                    } else {
                        self.state_manager.set_relay_enabled(false).await;
                    }
                }
                HardwareEvent::SendScaleCommand(command) => {
                    info!("⚡ HARDWARE: Scale command {:?}", command);
                    if let Err(_) = self.scale_command_channel.try_send(command) {
                        warn!("Scale command channel full");
                    }
                }
                HardwareEvent::DisplayUpdate {
                    state: _display_state,
                } => {
                    debug!("⚡ HARDWARE: Display update");
                    // TODO: Update display when implemented
                }
                HardwareEvent::DisplayAlert { message, duration } => {
                    info!("⚡ HARDWARE: Display alert: {} for {:?}", message, duration);
                    // TODO: Show alert on display
                }
            }
        }
    }

    /// 🎯 HANDLE ALL SYSTEM EVENTS - PURE EVENT-DRIVEN DISPATCH!
    async fn handle_system_event(&mut self, event: SystemEvent) {
        match event {
            SystemEvent::Scale(scale_event) => {
                self.handle_scale_event(scale_event).await;
            }
            SystemEvent::User(user_event) => {
                self.handle_user_event(user_event).await;
            }
            SystemEvent::Brew(brew_event) => {
                self.handle_brew_event(brew_event).await;
            }
            SystemEvent::Time(time_event) => {
                self.handle_time_event(time_event).await;
            }
            SystemEvent::Safety(safety_event) => {
                self.handle_safety_event(safety_event).await;
            }
            SystemEvent::Network(network_event) => {
                self.handle_network_event(network_event).await;
            }
            SystemEvent::Hardware(_) => {
                // Hardware events are processed by dedicated task - ignore here
                debug!("Hardware event handled by dedicated task");
            }
        }
    }

    /// Get event publisher for methods that need to publish events
    fn get_event_publisher(&self) -> EventPublisher {
        self.event_bus.publisher()
    }

    /// Reset scale event detection (useful when reconnecting or troubleshooting)
    pub fn reset_scale_event_detection(&mut self) {
        info!("🔄 Resetting scale event detection state");
        self.scale_event_detector.reset();
    }

    /// Get current state from scale event detector
    pub fn get_scale_detector_state(&self) -> (bool, Option<f32>) {
        (
            self.scale_event_detector.is_timer_running(),
            self.scale_event_detector.get_stable_weight(),
        )
    }

    /// Get comprehensive system status including event detection
    pub async fn get_comprehensive_status(&self) -> ComprehensiveStatus {
        let (timer_running, stable_weight) = self.get_scale_detector_state();
        ComprehensiveStatus {
            system_state: self.brew_controller.get_system_state(),
            scale_detector_timer_running: timer_running,
            scale_detector_stable_weight: stable_weight,
            system_enabled: self.brew_controller.is_system_enabled(),
            state_manager_state: self.state_manager.get_full_state().await,
        }
    }

    /// 🎯 Handle scale events - weight changes, connections, button presses
    async fn handle_scale_event(&mut self, scale_event: ScaleEvent) {
        match scale_event {
            ScaleEvent::WeightChanged { data } => {
                info!(
                    "📊 Scale: {:.2}g, flow: {:.2}g/s",
                    data.weight_g, data.flow_rate_g_per_s
                );

                // 🕵️ INTELLIGENT EVENT DETECTION - Analyze raw data for patterns!
                let detected_events = self.scale_event_detector.process_data(&data);
                
                // Process any detected events through the event bus
                // Note: ScaleEventDetector runs regardless of system state, but the state machine
                // will ignore events when in SystemDisabled state (killswitch)
                for detected_event in detected_events {
                    info!("🔍 Detected scale event: {:?}", detected_event);
                    let system_event = SystemEvent::Scale(detected_event);
                    self.get_event_publisher().publish(system_event).await;
                }

                // Update state manager
                self.state_manager.update_scale_data(data.clone()).await;

                // Send to brewing state machine
                let brew_input = BrewInput::ScaleData(data);
                let outputs = self.brew_controller.handle_input(brew_input);

                // Process state machine outputs
                for output in outputs {
                    self.handle_brew_output(output).await;
                }
            }
            ScaleEvent::Connected { info } => {
                info!("🔗 Scale connected: {} {}", info.brand, info.model);
                self.state_manager.set_ble_connected(true).await;
                
                // Notify state machine of scale connection
                let brew_input = BrewInput::ScaleConnected;
                let outputs = self.brew_controller.handle_input(brew_input);
                for output in outputs {
                    self.handle_brew_output(output).await;
                }
            }
            ScaleEvent::Disconnected { reason } => {
                warn!("❌ Scale disconnected: {}", reason);
                self.state_manager.set_ble_connected(false).await;
                
                // Notify state machine of scale disconnection
                let brew_input = BrewInput::ScaleDisconnected;
                let outputs = self.brew_controller.handle_input(brew_input);
                for output in outputs {
                    self.handle_brew_output(output).await;
                }
            }
            ScaleEvent::ButtonPressed(button) => {
                info!("🔘 Scale button: {:?}", button);
                // Convert to user event
                let user_event = match button {
                    ScaleButton::Tare => UserEvent::TareScale,
                    ScaleButton::Timer => UserEvent::StartBrewing,
                    _ => return,
                };
                self.get_event_publisher().user_command(user_event).await;
            }
            ScaleEvent::TimerStarted { timestamp_ms } => {
                info!("⏱️ Scale timer started: {}ms", timestamp_ms);
                // Trigger brewing
                self.get_event_publisher()
                    .user_command(UserEvent::StartBrewing)
                    .await;
            }
            ScaleEvent::TimerStopped { timestamp_ms } => {
                info!("⏹️ Scale timer stopped: {}ms", timestamp_ms);
                self.get_event_publisher()
                    .user_command(UserEvent::StopBrewing)
                    .await;
            }
            _ => {}
        }
    }

    /// 👤 Handle user events - commands from web interface or scale buttons
    async fn handle_user_event(&mut self, user_event: UserEvent) {
        info!("👤 User: {:?}", user_event);

        match user_event.clone() {
            UserEvent::SetTargetWeight(weight) => {
                let mut config = self.state_manager.get_config().await;
                config.target_weight_g = weight;
                self.state_manager.update_config(config).await;
                self.brew_controller.set_target_weight(weight);
            }
            UserEvent::SetAutoTare(enabled) => {
                let mut config = self.state_manager.get_config().await;
                config.auto_tare = enabled;
                self.state_manager.update_config(config).await;
            }
            UserEvent::SetPredictiveStop(enabled) => {
                let mut config = self.state_manager.get_config().await;
                config.predictive_stop = enabled;
                self.state_manager.update_config(config).await;
            }
            UserEvent::EmergencyStop => {
                // Emergency stop bypasses state machine
                self.get_event_publisher()
                    .emergency_stop("User emergency stop".to_string())
                    .await;
                return;
            }
            _ => {}
        }

        // Send to brewing state machine
        let brew_input = BrewInput::UserCommand(user_event);
        let outputs = self.brew_controller.handle_input(brew_input);

        // Process state machine outputs
        for output in outputs {
            self.handle_brew_output(output).await;
        }
    }

    /// ☕ Handle brew events - state changes, milestones
    async fn handle_brew_event(&mut self, brew_event: BrewEvent) {
        match brew_event {
            BrewEvent::StateChanged { from, to } => {
                info!("🔄 Brew state: {:?} -> {:?}", from, to);
                self.state_manager.update_brew_state(to).await;
            }
            BrewEvent::Started { target_weight } => {
                info!("🚀 Brewing started! Target: {:.1}g", target_weight);
                self.state_manager
                    .add_log("Brewing started".to_string())
                    .await;
            }
            BrewEvent::TargetWeightReached { actual, target } => {
                info!("🎯 Target reached! {:.1}g / {:.1}g", actual, target);
            }
            BrewEvent::PredictiveStopTriggered {
                predicted_overshoot,
            } => {
                info!(
                    "🔮 Predictive stop! Predicted overshoot: {:.1}g",
                    predicted_overshoot
                );
            }
            BrewEvent::Finished {
                final_weight,
                duration_ms,
            } => {
                info!(
                    "✅ Brewing finished! {:.1}g in {}ms",
                    final_weight, duration_ms
                );
                self.state_manager
                    .add_log("Brewing finished".to_string())
                    .await;
            }
            BrewEvent::AutoTareTriggered { reason } => {
                info!("⚖️ Auto-tare: {}", reason);
            }
            _ => {}
        }
    }

    /// ⏰ Handle time events - ticks, timeouts
    async fn handle_time_event(&mut self, time_event: TimeEvent) {
        match time_event {
            TimeEvent::Tick => {
                // Periodic safety checks
                let current_state = self.state_manager.get_full_state().await;
                if self.safety_controller.should_emergency_stop(&current_state) {
                    self.get_event_publisher()
                        .emergency_stop("Safety check failed".to_string())
                        .await;
                }

                // Send tick to brewing state machine for time-based logic
                let tick_outputs = self.brew_controller.handle_input(BrewInput::Tick);
                for output in tick_outputs {
                    self.handle_brew_output(output).await;
                }

                // Check settling timeout (legacy - now handled by state machine)
                let settling_outputs = self.brew_controller.check_settling_timeout();
                for output in settling_outputs {
                    self.handle_brew_output(output).await;
                }
            }
            TimeEvent::SettlingTimeout => {
                info!("⏰ Settling timeout");
                let outputs = self
                    .brew_controller
                    .handle_input(BrewInput::SettlingTimeout);
                for output in outputs {
                    self.handle_brew_output(output).await;
                }
            }
            _ => {}
        }
    }

    /// 🚨 Handle safety events - emergency stops, alerts
    async fn handle_safety_event(&mut self, safety_event: SafetyEvent) {
        match safety_event {
            SafetyEvent::EmergencyStop { reason } => {
                error!("🚨 EMERGENCY STOP: {}", reason);

                // Force relay off immediately
                self.get_event_publisher().relay_off().await;

                // Force state machine to idle
                let outputs = self.brew_controller.emergency_stop();
                for output in outputs {
                    self.handle_brew_output(output).await;
                }

                self.state_manager.set_error(Some(reason)).await;
            }
            SafetyEvent::SystemAlert { level, message } => match level {
                AlertLevel::Critical | AlertLevel::Error => {
                    error!("🚨 {}: {}", level.as_str(), message)
                }
                AlertLevel::Warning => warn!("⚠️ {}: {}", level.as_str(), message),
                AlertLevel::Info => info!("ℹ️ {}: {}", level.as_str(), message),
            },
            _ => {}
        }
    }

    /// 🌐 Handle network events - WiFi, BLE connections
    async fn handle_network_event(&mut self, network_event: NetworkEvent) {
        match network_event {
            NetworkEvent::WifiConnected { ssid } => {
                info!("📶 WiFi connected: {}", ssid);
                self.state_manager.set_wifi_connected(true).await;
            }
            NetworkEvent::WifiDisconnected => {
                warn!("📶 WiFi disconnected");
                self.state_manager.set_wifi_connected(false).await;
            }
            NetworkEvent::BleConnected { device_name } => {
                info!("🔵 BLE connected: {}", device_name);
                self.state_manager.set_ble_connected(true).await;
            }
            NetworkEvent::BleDisconnected => {
                warn!("🔵 BLE disconnected");
                self.state_manager.set_ble_connected(false).await;
            }
            _ => {}
        }
    }

    /// 🔄 Convert legacy WebSocket commands to user events
    fn websocket_to_user_event(&self, command: WebSocketCommand) -> Option<UserEvent> {
        match command {
            WebSocketCommand::SetTargetWeight { weight } => {
                Some(UserEvent::SetTargetWeight(weight))
            }
            WebSocketCommand::SetAutoTare { enabled } => Some(UserEvent::SetAutoTare(enabled)),
            WebSocketCommand::SetPredictiveStop { enabled } => {
                Some(UserEvent::SetPredictiveStop(enabled))
            }
            WebSocketCommand::TareScale => Some(UserEvent::TareScale),
            WebSocketCommand::StartTimer => Some(UserEvent::StartBrewing),
            WebSocketCommand::StopTimer => Some(UserEvent::StopBrewing),
            WebSocketCommand::ResetTimer => Some(UserEvent::ResetTimer),
            WebSocketCommand::TestRelay => Some(UserEvent::TestRelay),
            WebSocketCommand::ResetOvershoot => Some(UserEvent::ResetOvershoot),
        }
    }

    async fn handle_scale_data(&mut self, scale_data: ScaleData) {
        debug!(
            "Received scale data: {:.2}g, {:.2}g/s, timestamp: {}ms",
            scale_data.weight_g, scale_data.flow_rate_g_per_s, scale_data.timestamp_ms
        );

        self.safety_controller.update_data_received();
        self.state_manager
            .update_scale_data(scale_data.clone())
            .await;

        // Handle timer detection using Python reference logic
        self.handle_timer_detection(&scale_data).await;

        // Handle brewing state transitions through the new BrewController
        let brew_input = BrewInput::ScaleData(scale_data.clone());
        let outputs = self.brew_controller.handle_input(brew_input);

        // Process outputs from state machine
        for output in outputs {
            self.handle_brew_output(output).await;
        }

        // Handle auto-tare logic - call on every weight reading like Python
        if self.state_manager.is_auto_tare_enabled().await {
            let brew_state = self.state_manager.get_brew_state().await;
            let timer_state = self.state_manager.get_timer_state().await;
            let timer_running = timer_state == TimerState::Running;

            // Auto-tare logic is now handled inside the state machine
        }

        if self.state_manager.get_timer_state().await == TimerState::Running {
            self.handle_brewing_logic(&scale_data).await;
        }
    }

    async fn handle_brewing_logic(&mut self, scale_data: &ScaleData) {
        if !self.state_manager.is_predictive_stop_enabled().await {
            return;
        }

        // Skip predictive logic during startup delay (button press artifacts)
        if let Some(brew_start) = self.brew_start_time {
            let elapsed = Instant::now().duration_since(brew_start);
            if elapsed < Duration::from_millis(2000) {
                // 2 second delay
                debug!(
                    "Ignoring weight measurement during startup delay: {:.2}g ({}ms elapsed)",
                    scale_data.weight_g,
                    elapsed.as_millis()
                );
                return;
            }
        }

        let target_weight = self.state_manager.get_target_weight().await;

        // Handle auto-stop logic like Python
        self.handle_auto_stop(scale_data, target_weight).await;

        // Overshoot recording is now handled inside the state machine
    }

    /// Handle automatic stopping logic (from Python)
    async fn handle_auto_stop(&mut self, scale_data: &ScaleData, target_weight: f32) {
        // Target reached (with startup delay)
        if scale_data.weight_g >= target_weight && scale_data.timestamp_ms > 1000 {
            info!(
                "🎯 Target reached: {:.1}g >= {:.1}g at {}ms",
                scale_data.weight_g, target_weight, scale_data.timestamp_ms
            );
            // LEGACY: Direct brewing control replaced by state machine
            self.stop_brewing_with_reason("target_reached").await;
            return;
        }

        // Predictive stopping is now handled by the state machine
        debug!("Legacy predictive stop logic - state machine handles this now");
    }

    /// Schedule a delayed stop (Python equivalent of asyncio.create_task)
    async fn schedule_delayed_stop(&mut self, delay_seconds: f32) {
        // Delayed stops are now handled by the state machine with tick events
        debug!("Legacy delayed stop called - state machine handles this now");
    }

    async fn handle_brew_state_transition(&mut self, transition: BrewStateTransition) {
        match (transition.from, transition.to) {
            (BrewState::Idle, BrewState::Brewing) => {
                info!("🔥 Brewing started - activating relay immediately (robust timer detection)");
                self.brew_start_time = Some(Instant::now());

                // Activate relay immediately - no delay needed with proper timer detection
                if let Err(e) = self.relay_controller.turn_on().await {
                    error!("Failed to turn on relay: {:?}", e);
                    self.emergency_stop().await;
                } else {
                    self.state_manager.set_relay_enabled(true).await;
                }
            }
            (BrewState::Brewing, BrewState::BrewSettling) => {
                info!("Brewing finished, settling");
                self.brew_start_time = None; // Clear startup delay
                if let Err(e) = self.relay_controller.turn_off().await {
                    error!("Failed to turn off relay: {:?}", e);
                } else {
                    self.state_manager.set_relay_enabled(false).await;
                }
            }
            (BrewState::BrewSettling, BrewState::Idle) => {
                info!("Returned to idle state");
            }
            _ => {}
        }
    }

    async fn handle_ble_status_change(&mut self, connected: bool) {
        self.state_manager.set_ble_connected(connected).await;

        if !connected {
            // Check if disconnection happened shortly after timer start - indicates scale shutdown
            if let Some(timer_start) = self.timer_start_time {
                let elapsed = Instant::now().duration_since(timer_start);
                if elapsed < Duration::from_secs(3) {
                    // 3 second window
                    self.consecutive_disconnection_count += 1;
                    info!("BLE disconnected {}ms after timer start - potential scale shutdown (count: {})",
                         elapsed.as_millis(), self.consecutive_disconnection_count);

                    // If timer is running and we disconnect quickly, likely a shutdown - stop the timer
                    if self.current_timer_running {
                        info!("Scale shutdown detected - stopping timer");
                        self.current_timer_running = false;
                        self.state_manager
                            .update_timer_state(TimerState::Idle)
                            .await;

                        // Emergency stop if brewing was triggered
                        if self.state_manager.get_brew_state().await != BrewState::Idle {
                            info!("Emergency stop due to scale shutdown during brewing");
                            self.emergency_stop().await;
                        }
                    }
                } else {
                    // Reset disconnection count if enough time has passed
                    self.consecutive_disconnection_count = 0;
                }
            }

            self.state_manager
                .set_error(Some("BLE disconnected".to_string()))
                .await;
        } else {
            // Reset disconnection tracking on successful connection
            self.consecutive_disconnection_count = 0;
            self.timer_start_time = None;
            self.state_manager.set_error(None).await;
        }
    }

    async fn handle_websocket_command(&mut self, command: WebSocketCommand) {
        debug!("Received WebSocket command: {:?}", command);

        match command {
            WebSocketCommand::SetTargetWeight { weight } => {
                let mut config = self.state_manager.get_config().await;
                config.target_weight_g = weight;
                self.state_manager.update_config(config).await;

                // Update target weight in the brewing state machine
                self.brew_controller.set_target_weight(weight);

                info!("Target weight set to {:.1}g", weight);
            }

            WebSocketCommand::SetAutoTare { enabled } => {
                let mut config = self.state_manager.get_config().await;
                config.auto_tare = enabled;
                self.state_manager.update_config(config).await;
                info!(
                    "Auto-tare: {}",
                    if enabled { "enabled" } else { "disabled" }
                );
            }

            WebSocketCommand::SetPredictiveStop { enabled } => {
                let mut config = self.state_manager.get_config().await;
                config.predictive_stop = enabled;
                self.state_manager.update_config(config).await;
                info!(
                    "Predictive stop: {}",
                    if enabled { "enabled" } else { "disabled" }
                );
            }

            WebSocketCommand::TestRelay => {
                if let Err(e) = self.relay_controller.test_relay().await {
                    warn!("Relay test failed: {:?}", e);
                    self.state_manager
                        .add_log("Relay test failed".to_string())
                        .await;
                } else {
                    self.state_manager
                        .add_log("Relay test completed successfully".to_string())
                        .await;
                }
            }

            WebSocketCommand::TareScale => {
                // Route through state machine instead of direct command
                let outputs = self.brew_controller.handle_input(BrewInput::UserCommand(UserEvent::TareScale));
                for output in outputs {
                    self.handle_brew_output(output).await;
                }
                self.state_manager
                    .add_log("Tare command routed through state machine".to_string())
                    .await;
            }

            WebSocketCommand::StartTimer => {
                // Route through state machine instead of direct command
                let outputs = self.brew_controller.handle_input(BrewInput::UserCommand(UserEvent::StartBrewing));
                for output in outputs {
                    self.handle_brew_output(output).await;
                }
                self.state_manager
                    .add_log("Start brewing command routed through state machine".to_string())
                    .await;
            }

            WebSocketCommand::StopTimer => {
                // Route through state machine instead of direct command
                let outputs = self.brew_controller.handle_input(BrewInput::UserCommand(UserEvent::StopBrewing));
                for output in outputs {
                    self.handle_brew_output(output).await;
                }
                self.state_manager
                    .add_log("Stop brewing command routed through state machine".to_string())
                    .await;
            }

            WebSocketCommand::ResetTimer => {
                // Route through state machine instead of direct command
                let outputs = self.brew_controller.handle_input(BrewInput::UserCommand(UserEvent::ResetTimer));
                for output in outputs {
                    self.handle_brew_output(output).await;
                }
                self.state_manager
                    .add_log("Reset timer command routed through state machine".to_string())
                    .await;
            }

            WebSocketCommand::ResetOvershoot => {
                info!("🔄 User requested overshoot reset - forwarding to state machine");
                let user_event = UserEvent::ResetOvershoot;
                let brew_input = BrewInput::UserCommand(user_event);
                let outputs = self.brew_controller.handle_input(brew_input);
                for output in outputs {
                    self.handle_brew_output(output).await;
                }
            }
        }
    }

    async fn periodic_update(&mut self) {
        let current_state = self.state_manager.get_full_state().await;

        if self.safety_controller.should_emergency_stop(&current_state) {
            self.emergency_stop().await;
        }

        self.safety_controller
            .update_relay_state(current_state.relay_enabled);

        // Check for pending predictive stop (like Python's delayed task)
        if let Some(stop_time) = self.pending_stop_time {
            if Instant::now() >= stop_time {
                info!(
                    "⏰ EXECUTING DELAYED PREDICTIVE STOP (scheduled for {:?})",
                    stop_time
                );
                self.pending_stop_time = None;

                if self.state_manager.get_timer_state().await == TimerState::Running {
                    // LEGACY: Direct brewing control replaced by state machine
                    self.stop_brewing_with_reason("predicted").await;
                } else {
                    info!("Predictive stop cancelled - timer no longer running");
                }
            }
        }
    }

    async fn stop_brewing(&mut self) {
        self.stop_brewing_with_reason("manual").await
    }

    async fn stop_brewing_with_reason(&mut self, reason: &str) {
        info!("Stopping brewing ({})", reason);

        // Cancel pending stop task
        if self.pending_stop_time.is_some() {
            self.pending_stop_time = None;
        }

        // Overshoot learning is now handled by the state machine
        debug!("Overshoot learning now handled by state machine");

        // LEGACY: Direct scale commands removed - now handled by state machine
        // The state machine now handles all scale commands as side effects
        // if reason == "target_reached" || reason == "predicted" {
        //     if let Err(_) = self.scale_command_channel.try_send(ScaleCommand::StopTimer) {
        //         warn!("Failed to send stop timer command - channel full");
        //     }
        // }

        // LEGACY: Direct relay control removed - now handled by state machine
        // self.relay_controller.turn_off().await?;
        // self.state_manager.set_relay_enabled(false).await;
        self.state_manager
            .add_log(format!("Brewing stopped ({})", reason))
            .await;
    }

    async fn emergency_stop(&mut self) {
        error!("EMERGENCY STOP ACTIVATED");

        self.brew_start_time = None; // Clear startup delay
        self.pending_stop_time = None; // Cancel any pending predictive stops

        self.safety_controller
            .handle_emergency_stop(&mut self.relay_controller);
        self.state_manager.set_relay_enabled(false).await;
        self.state_manager
            .set_error(Some("Emergency stop activated".to_string()))
            .await;
        self.state_manager
            .add_log("EMERGENCY STOP".to_string())
            .await;

        // TODO: Replace with proper BrewController emergency stop
        // self.brew_controller.emergency_stop();
        self.state_manager.update_brew_state(BrewState::Idle).await;
        self.state_manager
            .update_timer_state(TimerState::Idle)
            .await;
    }

    /// Handle timer state detection from scale data (Python reference implementation)
    async fn handle_timer_detection(&mut self, scale_data: &ScaleData) {
        if self.last_timer_ms.is_none() {
            self.last_timer_ms = Some(scale_data.timestamp_ms);
            return;
        }

        let last_timer_ms = self.last_timer_ms.unwrap();

        // Timer started: not running + timestamp > 0 + timestamp > last_timestamp
        if !self.current_timer_running
            && scale_data.timestamp_ms > 0
            && scale_data.timestamp_ms > last_timer_ms
        {
            // Check if this could be a scale shutdown (timer start followed by immediate disconnection)
            // If we've had recent disconnections after a timer start, ignore this timer start
            if self.consecutive_disconnection_count > 0 {
                info!("Ignoring timer start - likely scale shutdown sequence (disconnection count: {})",
                     self.consecutive_disconnection_count);
                self.consecutive_disconnection_count = 0;
                return;
            }

            // Additional shutdown detection: if weight jumps dramatically with very high flow rate,
            // this is likely the shutdown sequence where scale readings go crazy before disconnecting
            if scale_data.flow_rate_g_per_s > 50.0 {
                info!("Ignoring timer start - extreme flow rate {:.1}g/s indicates scale shutdown sequence",
                     scale_data.flow_rate_g_per_s);
                // Don't set timer running, just wait for disconnect
                return;
            }

            // Also check for rapid weight increases that are physically impossible during normal brewing
            // Lower threshold to catch more shutdown sequences
            if scale_data.flow_rate_g_per_s > 25.0 {
                info!("Ignoring timer start - high flow rate {:.1}g/s indicates scale shutdown sequence",
                     scale_data.flow_rate_g_per_s);
                return;
            }

            info!(
                "Timer started detected: {}ms -> {}ms",
                last_timer_ms, scale_data.timestamp_ms
            );
            self.current_timer_running = true;
            self.timer_start_time = Some(Instant::now());
            self.state_manager
                .update_timer_state(TimerState::Running)
                .await;
        }
        // Timer stopped manually - IMMEDIATE DETECTION LIKE PYTHON
        else if self.current_timer_running
            && scale_data.timestamp_ms == last_timer_ms
            && scale_data.timestamp_ms > 0
        {
            info!(
                "⏹️ Timer stopped manually: timestamp frozen at {}ms (IMMEDIATE DETECTION)",
                scale_data.timestamp_ms
            );
            self.current_timer_running = false;
            self.state_manager
                .update_timer_state(TimerState::Idle)
                .await;
        }
        // Timer reset
        else if self.current_timer_running && scale_data.timestamp_ms == 0 {
            info!("Timer reset detected: timestamp -> 0");
            self.current_timer_running = false;
            self.state_manager
                .update_timer_state(TimerState::Idle)
                .await;
        }

        // Update last timestamp - AFTER detection logic like Python
        self.last_timer_ms = Some(scale_data.timestamp_ms);
    }

    /// 🚀 Handle outputs from the brewing state machine - PURE SIDE EFFECTS!
    /// State machine decides, events drive hardware - no direct hardware calls!
    async fn handle_brew_output(&mut self, output: BrewOutput) {
        match output {
            BrewOutput::RelayOn => {
                info!("🔥 State machine output: RelayOn -> Publishing hardware event");
                self.get_event_publisher().relay_on().await;
                self.state_manager.set_relay_enabled(true).await;
            }
            BrewOutput::RelayOff => {
                info!("⏹️ State machine output: RelayOff -> Publishing hardware event");
                self.get_event_publisher().relay_off().await;
                self.state_manager.set_relay_enabled(false).await;
            }
            BrewOutput::StateChanged { from, to } => {
                info!("🔄 Brew state transition: {:?} -> {:?}", from, to);
                // Convert SystemState to BrewState for legacy state manager
                let brew_state = match to {
                    crate::brewing::states::SystemState::Idle => crate::types::BrewState::Idle,
                    crate::brewing::states::SystemState::Brewing => {
                        crate::types::BrewState::Brewing
                    }
                    crate::brewing::states::SystemState::Settling => {
                        crate::types::BrewState::BrewSettling
                    }
                    _ => crate::types::BrewState::Idle,
                };
                self.state_manager.update_brew_state(brew_state).await;
            }
            BrewOutput::TareScale => {
                info!("⚖️ State machine output: TareScale -> Publishing hardware event");
                self.get_event_publisher()
                    .publish(SystemEvent::Hardware(HardwareEvent::SendScaleCommand(
                        ScaleCommand::Tare,
                    )))
                    .await;
            }
            BrewOutput::StartTimer => {
                info!("▶️ State machine output: StartTimer -> Publishing hardware event");
                self.get_event_publisher()
                    .publish(SystemEvent::Hardware(HardwareEvent::SendScaleCommand(
                        ScaleCommand::StartTimer,
                    )))
                    .await;
            }
            BrewOutput::StopTimer => {
                info!("⏹️ State machine output: StopTimer -> Publishing hardware event");
                self.get_event_publisher()
                    .publish(SystemEvent::Hardware(HardwareEvent::SendScaleCommand(
                        ScaleCommand::StopTimer,
                    )))
                    .await;
            }
            BrewOutput::ResetTimer => {
                info!("🔄 State machine output: ResetTimer -> Publishing hardware event");
                self.get_event_publisher()
                    .publish(SystemEvent::Hardware(HardwareEvent::SendScaleCommand(
                        ScaleCommand::ResetTimer,
                    )))
                    .await;
            }
            BrewOutput::BrewingStarted => {
                info!("☕ Brewing started");
                self.state_manager
                    .add_log("Brewing started".to_string())
                    .await;
            }
            BrewOutput::BrewingFinished => {
                info!("✅ Brewing finished");
                self.state_manager
                    .add_log("Brewing finished".to_string())
                    .await;
            }
            BrewOutput::PredictiveStopTriggered => {
                info!("🎯 Predictive stop triggered");
                self.state_manager
                    .add_log("Predictive stop triggered".to_string())
                    .await;
            }
            BrewOutput::DisplayUpdate => {
                // Display updates handled elsewhere for now
                debug!("Display update requested");
            }
            BrewOutput::SystemEnabled => {
                info!("✅ System enabled - killswitch OFF");
                // Could publish system status event if needed
            }
            BrewOutput::SystemDisabled => {
                info!("🚫 System disabled - killswitch ON");
                // Could publish system status event if needed
            }
            BrewOutput::ScaleConnectionChanged { connected } => {
                info!(
                    "🔗 Scale connection changed: {}",
                    if connected {
                        "Connected"
                    } else {
                        "Disconnected"
                    }
                );
                self.state_manager.set_ble_connected(connected).await;
            }
            BrewOutput::EnableBle => {
                info!("🔌 State machine output: EnableBle -> Publishing hardware event");
                // TODO: Implement BLE enable event
                self.state_manager
                    .add_log("BLE enabled".to_string())
                    .await;
            }
            BrewOutput::DisableBle => {
                info!("🔌 State machine output: DisableBle -> Publishing hardware event");
                // TODO: Implement BLE disable event
                self.state_manager
                    .add_log("BLE disabled".to_string())
                    .await;
            }
            BrewOutput::StartBleScanning => {
                info!("🔍 State machine output: StartBleScanning -> Publishing hardware event");
                // TODO: Implement BLE scanning event
                self.state_manager
                    .add_log("BLE scanning started".to_string())
                    .await;
            }
            BrewOutput::StopBleScanning => {
                info!("🔍 State machine output: StopBleScanning -> Publishing hardware event");
                // TODO: Implement BLE stop scanning event
                self.state_manager
                    .add_log("BLE scanning stopped".to_string())
                    .await;
            }
            BrewOutput::ConnectToWifi { ssid, password } => {
                info!("📡 State machine output: ConnectToWifi -> Publishing hardware event");
                // TODO: Implement WiFi connect event
                self.state_manager
                    .add_log(format!("WiFi connecting to {}", ssid))
                    .await;
            }
            BrewOutput::DisconnectWifi => {
                info!("📡 State machine output: DisconnectWifi -> Publishing hardware event");
                // TODO: Implement WiFi disconnect event
                self.state_manager
                    .add_log("WiFi disconnected".to_string())
                    .await;
            }
            BrewOutput::StartWifiProvisioning => {
                info!("📡 State machine output: StartWifiProvisioning -> Publishing hardware event");
                // TODO: Implement WiFi provisioning event
                self.state_manager
                    .add_log("WiFi provisioning started".to_string())
                    .await;
            }
            BrewOutput::NetworkStatusChanged { ble_enabled, wifi_connected } => {
                info!("🌐 Network status changed: BLE={}, WiFi={}", ble_enabled, wifi_connected);
                self.state_manager.set_ble_connected(ble_enabled).await;
                // TODO: Add wifi status to state manager
                self.state_manager
                    .add_log(format!("Network status: BLE={}, WiFi={}", ble_enabled, wifi_connected))
                    .await;
            }
            BrewOutput::AutoTareStateChanged { from, to } => {
                info!("🔄 Auto-tare state transition: {:?} -> {:?}", from, to);
                self.state_manager.update_auto_tare_state(to).await;
            }
            BrewOutput::AutoTareExecuted => {
                info!("⚖️ Auto-tare executed by state machine");
                self.state_manager
                    .add_log("Auto-tare executed".to_string())
                    .await;
            }
            BrewOutput::PredictiveStopScheduled { delay_ms, predicted_weight } => {
                info!("🎯 Predictive stop scheduled: delay={}ms, predicted_weight={:.1}g", delay_ms, predicted_weight);
                self.state_manager
                    .add_log(format!("Predictive stop scheduled: {}ms delay", delay_ms))
                    .await;
            }
            BrewOutput::OvershootLearningUpdated { delay_ms, ewma, confidence } => {
                info!("📊 Overshoot learning updated: delay={}ms, ewma={:.1}g, confidence={:.1}%", 
                      delay_ms, ewma, confidence * 100.0);
                self.state_manager
                    .add_log(format!("Overshoot learning: delay={}ms, ewma={:.1}g", delay_ms, ewma))
                    .await;
            }
            BrewOutput::OvershootControllerReset => {
                info!("🔄 Overshoot controller reset");
                self.state_manager
                    .add_log("Overshoot controller reset".to_string())
                    .await;
            }
            BrewOutput::StartWifiProvisioning => {
                info!("📱 State machine output: StartWifiProvisioning -> Starting WiFi provisioning");
                // TODO: Implement WiFi provisioning start
            }
            BrewOutput::StopWifiProvisioning => {
                info!("📱 State machine output: StopWifiProvisioning -> Stopping WiFi provisioning");
                // TODO: Implement WiFi provisioning stop
            }
            BrewOutput::WifiProvisioningStatusChanged { active, device_name } => {
                info!("📱 WiFi provisioning status: active={}, device={:?}", active, device_name);
                // TODO: Update UI with provisioning status
            }
            BrewOutput::ResetWifiCredentials => {
                info!("🔄 State machine output: ResetWifiCredentials -> Resetting WiFi credentials");
                // TODO: Implement WiFi credentials reset
            }
            BrewOutput::ConnectToWifi { ssid, password } => {
                info!("📶 State machine output: ConnectToWifi -> Connecting to SSID: {}", ssid);
                // TODO: Implement WiFi connection with credentials
            }
            BrewOutput::DisconnectWifi => {
                info!("📶 State machine output: DisconnectWifi -> Disconnecting from WiFi");
                // TODO: Implement WiFi disconnection
            }
        }
    }
}

// Embassy task functions
#[embassy_executor::task]
async fn scale_task(mut scale_client: BookooScale, command_channel: Arc<ScaleCommandChannel>) {
    info!("Scale task started with command channel");

    // Start scale client with command channel support
    if let Err(e) = scale_client.start_with_commands(command_channel).await {
        error!("Scale task error: {:?}", e);
    }
}

#[embassy_executor::task]
async fn scale_data_bridge_task(
    scale_data_channel: Arc<ScaleDataChannel>,
    ble_status_channel: Arc<StatusChannel>,
    event_bus: Arc<EventBus>,
) {
    info!("🌉 Scale data bridge task started - connecting scale data to event bus");
    
    let event_publisher = event_bus.publisher();
    
    loop {
        let scale_data_fut = scale_data_channel.receive();
        let ble_status_fut = ble_status_channel.receive();
        
        match select(scale_data_fut, ble_status_fut).await {
            Either::First(scale_data) => {
                // Convert scale data to scale event and publish
                event_publisher
                    .publish(SystemEvent::Scale(ScaleEvent::WeightChanged { data: scale_data }))
                    .await;
            }
            Either::Second(ble_connected) => {
                // Convert BLE status to network event and publish
                if ble_connected {
                    event_publisher
                        .publish(SystemEvent::Network(NetworkEvent::BleConnected { 
                            device_name: "Bookoo Scale".to_string() 
                        }))
                        .await;
                } else {
                    event_publisher
                        .publish(SystemEvent::Network(NetworkEvent::BleDisconnected))
                        .await;
                }
            }
        }
    }
}

#[embassy_executor::task]
async fn websocket_task(websocket_server: WebSocketServer) {
    info!("WebSocket/HTTP task started");
    if let Err(e) = websocket_server.start().await {
        warn!(
            "WebSocket/HTTP server failed to start: {:?} - continuing without web interface",
            e
        );
        // Return instead of panicking - BLE functionality can continue
    } else {
        info!("WebSocket/HTTP server started successfully");
    }
}

// NOTE: Hardware side effects and tick events are now processed directly
// in the main event loop to avoid embassy task lifetime and generic issues
