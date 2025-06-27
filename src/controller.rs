use crate::{
    auto_tare::AutoTareController,
    ble::StatusChannel,
    bookoo_scale::{BookooScale, ScaleDataChannel},
    brew_states::{BrewStateMachine, BrewStateTransition},
    overshoot::{OvershootController, StopTiming},
    relay::{RelayController, RelayError},
    safety::SafetyController,
    state::StateManager,
    types::{BrewConfig, BrewState, ScaleData, SystemState, TimerState},
    websocket::{WebSocketCommand, WebSocketCommandChannel, WebSocketServer},
};
use embassy_executor::Spawner;
use embassy_futures::select::{select, Either};
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, channel::Channel, mutex::Mutex};
use embassy_time::{Duration, Instant, Timer};
// BLE now handled by esp32-nimble crate
use esp_idf_svc::hal::gpio::Gpio19;
use log::{debug, error, info, warn};
use std::sync::Arc;

// Scale command types for BLE command channel
#[derive(Debug, Clone)]
pub enum ScaleCommand {
    Tare,
    StartTimer,
    StopTimer,
    ResetTimer,
}

pub type ScaleCommandChannel = Channel<CriticalSectionRawMutex, ScaleCommand, 5>;

pub struct EspressoController {
    state_manager: StateManager,
    scale_client: BookooScale,
    websocket_server: WebSocketServer,
    relay_controller: RelayController,
    safety_controller: SafetyController,
    auto_tare_controller: AutoTareController,
    brew_state_machine: BrewStateMachine,
    overshoot_controller: OvershootController,

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
    timer_start_time: Option<Instant>, // When timer was started
    consecutive_disconnection_count: u32, // Count BLE disconnections after timer start
    
    // Brewing startup delay to ignore button press artifacts
    brew_start_time: Option<Instant>,
    
    // Flag to handle auto-tare after brewing finishes
    just_finished_brewing: bool,
}

impl EspressoController {
    pub fn new(gpio19: Gpio19) -> Result<Self, Box<dyn std::error::Error>> {
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

        Ok(Self {
            state_manager,
            scale_client,
            websocket_server,
            relay_controller,
            safety_controller: SafetyController::new(),
            auto_tare_controller: AutoTareController::new(),
            brew_state_machine: BrewStateMachine::new(),
            overshoot_controller: OvershootController::new(),

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
            
            // Auto-tare state
            just_finished_brewing: false,
        })
    }

    pub async fn start(&mut self, spawner: Spawner) -> Result<(), Box<dyn std::error::Error>> {
        info!("Starting Espresso Controller with Embassy tasks");

        // Initialize BLE stack
        BookooScale::initialize()?;

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
            .spawn(scale_task(scale_client, Arc::clone(&self.scale_command_channel)))
            .map_err(|_| "Failed to spawn scale task")?;

        // Spawn WebSocket/HTTP server task (non-fatal if it fails)
        if let Err(_) = spawner.spawn(websocket_task(websocket_server)) {
            warn!("Failed to spawn WebSocket task - continuing without HTTP server");
        }

        // Run the main control loop
        self.main_control_loop().await;

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

        if let Some(transition) = self
            .brew_state_machine
            .update(&scale_data, self.state_manager.get_timer_state().await)
        {
            self.state_manager.update_brew_state(transition.to).await;
            self.handle_brew_state_transition(transition).await;
        }

        // Handle auto-tare logic - call on every weight reading like Python
        if self.state_manager.is_auto_tare_enabled().await {
            let brew_state = self.state_manager.get_brew_state().await;
            let timer_state = self.state_manager.get_timer_state().await;
            let timer_running = timer_state == TimerState::Running;
            
            // If we just finished brewing, inform auto-tare controller to preserve current object
            if self.just_finished_brewing && brew_state == BrewState::Idle {
                self.auto_tare_controller.brewing_finished(scale_data.weight_g);
                self.just_finished_brewing = false;
            }
            
            if self.auto_tare_controller.should_auto_tare(&scale_data, brew_state, timer_running) {
                info!("Auto-tare triggered - taring scale at {:.1}g", scale_data.weight_g);
                if let Err(_) = self.scale_command_channel.try_send(ScaleCommand::Tare) {
                    warn!("Failed to send auto-tare command - channel full");
                } else {
                    self.auto_tare_controller.record_tare();
                    
                    // Reset timer if needed (like Python)
                    if scale_data.timestamp_ms > 0 {
                        if let Err(_) = self.scale_command_channel.try_send(ScaleCommand::ResetTimer) {
                            warn!("Failed to send reset timer command - channel full");
                        }
                    }
                }
            }
            
            self.state_manager
                .update_auto_tare_state(self.auto_tare_controller.get_state())
                .await;
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
            if elapsed < Duration::from_millis(2000) { // 2 second delay
                debug!("Ignoring weight measurement during startup delay: {:.2}g ({}ms elapsed)", 
                       scale_data.weight_g, elapsed.as_millis());
                return;
            }
        }

        let target_weight = self.state_manager.get_target_weight().await;

        // Handle auto-stop logic like Python
        self.handle_auto_stop(scale_data, target_weight).await;
        
        // Record overshoot when flow stops after predicted stop
        self.overshoot_controller.record_overshoot(
            scale_data.weight_g, 
            target_weight, 
            scale_data.flow_rate_g_per_s
        );
    }
    
    /// Handle automatic stopping logic (from Python)
    async fn handle_auto_stop(&mut self, scale_data: &ScaleData, target_weight: f32) {
        // Target reached (with startup delay)
        if scale_data.weight_g >= target_weight && scale_data.timestamp_ms > 1000 {
            info!(
                "🎯 Target reached: {:.1}g >= {:.1}g at {}ms",
                scale_data.weight_g,
                target_weight,
                scale_data.timestamp_ms
            );
            if let Err(e) = self.stop_brewing_with_reason("target_reached").await {
                error!("Failed to stop brewing: {:?}", e);
                self.emergency_stop().await;
            }
            return;
        }

        // Predictive stopping (after startup period) - Python algorithm
        if scale_data.flow_rate_g_per_s > 0.0 && scale_data.timestamp_ms > 2000 {
            let weight_needed = target_weight - scale_data.weight_g;
            let time_to_target = weight_needed / scale_data.flow_rate_g_per_s;

            let (min_time, max_time) = self.overshoot_controller.calculate_prediction_window();

            // COMPREHENSIVE DEBUG LOGGING
            debug!(
                "PREDICTION CHECK: weight={:.1}g, target={:.1}g, needed={:.1}g, flow={:.1}g/s, time_to_target={:.1}s, window=[{:.1}s, {:.1}s], delay={}ms", 
                scale_data.weight_g, target_weight, weight_needed, scale_data.flow_rate_g_per_s, 
                time_to_target, min_time, max_time, self.overshoot_controller.get_current_delay_ms()
            );

            if min_time < time_to_target && time_to_target <= max_time {
                // Cancel existing prediction
                if self.pending_stop_time.is_some() {
                    info!("Cancelling previous prediction");
                }

                info!(
                    "🎯 PREDICTION TRIGGERED: target in {:.1}s, scheduling stop...", 
                    time_to_target
                );
                
                // Start delayed stop task
                self.schedule_delayed_stop(time_to_target).await;
            } else {
                // Log why prediction didn't trigger
                if time_to_target <= min_time {
                    debug!("Prediction rejected: time_to_target {:.1}s <= min_time {:.1}s (too close)", time_to_target, min_time);
                } else if time_to_target > max_time {
                    debug!("Prediction rejected: time_to_target {:.1}s > max_time {:.1}s (too far)", time_to_target, max_time);
                }
            }
        } else {
            // Log why predictive logic was skipped
            if scale_data.flow_rate_g_per_s <= 0.0 {
                debug!("Prediction skipped: flow_rate {:.1}g/s <= 0", scale_data.flow_rate_g_per_s);
            } else if scale_data.timestamp_ms <= 2000 {
                debug!("Prediction skipped: timestamp {}ms <= 2000ms (startup period)", scale_data.timestamp_ms);
            }
        }
    }
    
    /// Schedule a delayed stop (Python equivalent of asyncio.create_task)
    async fn schedule_delayed_stop(&mut self, delay_seconds: f32) {
        let compensated_delay = self.overshoot_controller.get_compensated_delay(delay_seconds);
        let delay_duration = Duration::from_millis((compensated_delay * 1000.0) as u64);
        
        self.pending_stop_time = Some(Instant::now() + delay_duration);
        
        info!("⏰ SCHEDULED STOP: in {:.1}s (compensated from {:.1}s), executing at {:?}", 
               compensated_delay, delay_seconds, self.pending_stop_time.unwrap());
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
                // Mark that we just finished brewing so auto-tare can preserve current object
                self.just_finished_brewing = true;
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
                if elapsed < Duration::from_secs(3) { // 3 second window
                    self.consecutive_disconnection_count += 1;
                    info!("BLE disconnected {}ms after timer start - potential scale shutdown (count: {})", 
                         elapsed.as_millis(), self.consecutive_disconnection_count);
                    
                    // If timer is running and we disconnect quickly, likely a shutdown - stop the timer
                    if self.current_timer_running {
                        info!("Scale shutdown detected - stopping timer");
                        self.current_timer_running = false;
                        self.state_manager.update_timer_state(TimerState::Idle).await;
                        
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
            WebSocketCommand::SetTargetWeight(weight) => {
                let mut config = self.state_manager.get_config().await;
                config.target_weight_g = weight;
                self.state_manager.update_config(config).await;
                info!("Target weight set to {:.1}g", weight);
            }

            WebSocketCommand::SetAutoTare(enabled) => {
                let mut config = self.state_manager.get_config().await;
                config.auto_tare = enabled;
                self.state_manager.update_config(config).await;
                info!(
                    "Auto-tare: {}",
                    if enabled { "enabled" } else { "disabled" }
                );
            }

            WebSocketCommand::SetPredictiveStop(enabled) => {
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
                if let Err(_) = self.scale_command_channel.try_send(ScaleCommand::Tare) {
                    warn!("Failed to send tare command - channel full");
                    self.state_manager
                        .add_log("Failed to send tare command".to_string())
                        .await;
                } else {
                    self.state_manager
                        .add_log("Tare command sent to scale".to_string())
                        .await;
                }
            }

            WebSocketCommand::StartTimer => {
                if let Err(_) = self.scale_command_channel.try_send(ScaleCommand::StartTimer) {
                    warn!("Failed to send start timer command - channel full");
                } else {
                    self.state_manager
                        .add_log("Start timer command sent to scale".to_string())
                        .await;
                }
            }

            WebSocketCommand::StopTimer => {
                if let Err(e) = self.stop_brewing().await {
                    error!("Failed to stop brewing: {:?}", e);
                }
            }

            WebSocketCommand::ResetTimer => {
                if let Err(_) = self.scale_command_channel.try_send(ScaleCommand::ResetTimer) {
                    warn!("Failed to send reset timer command - channel full");
                } else {
                    self.state_manager
                        .add_log("Reset timer command sent to scale".to_string())
                        .await;
                }
                self.brew_state_machine.force_idle();
                self.state_manager.update_brew_state(BrewState::Idle).await;
            }

            WebSocketCommand::ResetOvershoot => {
                self.overshoot_controller.reset();
                self.state_manager
                    .add_log("Overshoot controller reset".to_string())
                    .await;
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
                info!("⏰ EXECUTING DELAYED PREDICTIVE STOP (scheduled for {:?})", stop_time);
                self.pending_stop_time = None;
                
                if self.state_manager.get_timer_state().await == TimerState::Running {
                    if let Err(e) = self.stop_brewing_with_reason("predicted").await {
                        error!("Failed to execute predictive stop: {:?}", e);
                        self.emergency_stop().await;
                    }
                } else {
                    info!("Predictive stop cancelled - timer no longer running");
                }
            }
        }
    }

    async fn stop_brewing(&mut self) -> Result<(), RelayError> {
        self.stop_brewing_with_reason("manual").await
    }
    
    async fn stop_brewing_with_reason(&mut self, reason: &str) -> Result<(), RelayError> {
        info!("Stopping brewing ({})", reason);

        // Cancel pending stop task
        if self.pending_stop_time.is_some() {
            self.pending_stop_time = None;
        }

        // Mark predicted stop if this was automatic - also mark for target_reached if there was a pending prediction
        if reason == "predicted" {
            self.overshoot_controller.mark_predicted_stop();
        } else if reason == "target_reached" && self.pending_stop_time.is_some() {
            // Target was reached but we had a prediction scheduled - still count as predicted stop for learning
            info!("🎯 Target reached with pending prediction - marking for overshoot learning");
            self.overshoot_controller.mark_predicted_stop();
        }

        // Send stop command for automatic stops (like Python)
        if reason == "target_reached" || reason == "predicted" {
            if let Err(_) = self.scale_command_channel.try_send(ScaleCommand::StopTimer) {
                warn!("Failed to send stop timer command - channel full");
            }
        }

        self.relay_controller.turn_off().await?;
        self.state_manager.set_relay_enabled(false).await;
        self.state_manager
            .add_log(format!("Brewing stopped ({})", reason))
            .await;

        Ok(())
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

        self.brew_state_machine.force_idle();
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
            && scale_data.timestamp_ms > last_timer_ms {
            
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
            
            info!("Timer started detected: {}ms -> {}ms", last_timer_ms, scale_data.timestamp_ms);
            self.current_timer_running = true;
            self.timer_start_time = Some(Instant::now());
            self.state_manager.update_timer_state(TimerState::Running).await;
        }
        
        // Timer stopped manually - IMMEDIATE DETECTION LIKE PYTHON
        else if self.current_timer_running 
            && scale_data.timestamp_ms == last_timer_ms 
            && scale_data.timestamp_ms > 0 {
            info!("⏹️ Timer stopped manually: timestamp frozen at {}ms (IMMEDIATE DETECTION)", scale_data.timestamp_ms);
            self.current_timer_running = false;
            self.state_manager.update_timer_state(TimerState::Idle).await;
        }
        
        // Timer reset
        else if self.current_timer_running && scale_data.timestamp_ms == 0 {
            info!("Timer reset detected: timestamp -> 0");
            self.current_timer_running = false;
            self.state_manager.update_timer_state(TimerState::Idle).await;
        }

        // Update last timestamp - AFTER detection logic like Python
        self.last_timer_ms = Some(scale_data.timestamp_ms);
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
async fn websocket_task(websocket_server: WebSocketServer) {
    info!("WebSocket/HTTP task started");
    if let Err(e) = websocket_server.start().await {
        warn!("WebSocket/HTTP server failed to start: {:?} - continuing without web interface", e);
        // Return instead of panicking - BLE functionality can continue
    } else {
        info!("WebSocket/HTTP server started successfully");
    }
}
