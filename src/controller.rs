use crate::{
    auto_tare::{AutoTareController, TareAction},
    ble::{BleScaleClient, BleStatusChannel, ScaleDataChannel},
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

pub struct EspressoController {
    state_manager: StateManager,
    ble_client: BleScaleClient,
    websocket_server: WebSocketServer,
    relay_controller: RelayController,
    safety_controller: SafetyController,
    auto_tare_controller: AutoTareController,
    brew_state_machine: BrewStateMachine,
    overshoot_controller: OvershootController,

    scale_data_channel: Arc<ScaleDataChannel>,
    ble_status_channel: Arc<BleStatusChannel>,
    websocket_command_channel: Arc<WebSocketCommandChannel>,

    last_stop_prediction: Option<StopTiming>,
    prediction_timer_start: Option<Instant>,
}

impl EspressoController {
    pub fn new(gpio19: Gpio19) -> Result<Self, Box<dyn std::error::Error>> {
        let scale_data_channel = Arc::new(Channel::new());
        let ble_status_channel = Arc::new(Channel::new());
        let websocket_command_channel = Arc::new(Channel::new());

        let state_manager = StateManager::new();
        let state_handle = state_manager.get_state_handle();

        let ble_client = BleScaleClient::new(
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
            ble_client,
            websocket_server,
            relay_controller,
            safety_controller: SafetyController::new(),
            auto_tare_controller: AutoTareController::new(),
            brew_state_machine: BrewStateMachine::new(),
            overshoot_controller: OvershootController::new(),

            scale_data_channel,
            ble_status_channel,
            websocket_command_channel,

            last_stop_prediction: None,
            prediction_timer_start: None,
        })
    }

    pub async fn start(&mut self, spawner: Spawner) -> Result<(), Box<dyn std::error::Error>> {
        info!("Starting Espresso Controller with Embassy tasks");

        // Clone references for the tasks
        let ble_client = self.ble_client.clone();
        let websocket_server = self.websocket_server.clone();
        let _state_handle = self.state_manager.get_state_handle();

        // Spawn BLE task
        spawner
            .spawn(ble_task(ble_client))
            .map_err(|_| "Failed to spawn BLE task")?;

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
            "Received scale data: {:.2}g, {:.2}g/s",
            scale_data.weight_g, scale_data.flow_rate_g_per_s
        );

        self.safety_controller.update_data_received();
        self.state_manager
            .update_scale_data(scale_data.clone())
            .await;

        if scale_data.timer_running {
            if self.state_manager.get_timer_state().await != TimerState::Running {
                self.state_manager
                    .update_timer_state(TimerState::Running)
                    .await;
            }
        } else {
            if self.state_manager.get_timer_state().await == TimerState::Running {
                self.state_manager
                    .update_timer_state(TimerState::Idle)
                    .await;
            }
        }

        if let Some(transition) = self
            .brew_state_machine
            .update(&scale_data, self.state_manager.get_timer_state().await)
        {
            self.state_manager.update_brew_state(transition.to).await;
            self.handle_brew_state_transition(transition).await;
        }

        if self.state_manager.is_auto_tare_enabled().await {
            if let Some(action) = self
                .auto_tare_controller
                .update(&scale_data, self.state_manager.get_brew_state().await)
            {
                self.handle_auto_tare_action(action).await;
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

        let target_weight = self.state_manager.get_target_weight().await;

        if let Some(timing) = self.overshoot_controller.calculate_stop_timing(
            scale_data.weight_g,
            target_weight,
            scale_data.flow_rate_g_per_s,
        ) {
            if timing.should_stop_now && self.last_stop_prediction.is_none() {
                info!(
                    "Predictive stop triggered at {:.2}g (target: {:.2}g, predicted: {:.2}g)",
                    scale_data.weight_g, target_weight, timing.predicted_final_weight
                );

                if let Err(e) = self.stop_brewing().await {
                    error!("Failed to stop brewing: {:?}", e);
                    self.emergency_stop().await;
                } else {
                    self.last_stop_prediction = Some(timing);
                    self.prediction_timer_start = Some(Instant::now());
                }
            }
        }

        if let Some(prediction_start) = self.prediction_timer_start {
            if Instant::now().duration_since(prediction_start) > Duration::from_secs(10) {
                if let Some(prediction) = &self.last_stop_prediction {
                    self.overshoot_controller
                        .record_actual_result(scale_data.weight_g);
                    info!(
                        "Recorded final weight for overshoot learning: {:.2}g",
                        scale_data.weight_g
                    );
                }
                self.last_stop_prediction = None;
                self.prediction_timer_start = None;
            }
        }
    }

    async fn handle_brew_state_transition(&mut self, transition: BrewStateTransition) {
        match (transition.from, transition.to) {
            (BrewState::Idle, BrewState::Brewing) => {
                info!("Brewing started");
                if let Err(e) = self.relay_controller.turn_on().await {
                    error!("Failed to turn on relay: {:?}", e);
                    self.emergency_stop().await;
                } else {
                    self.state_manager.set_relay_enabled(true).await;
                }
            }
            (BrewState::Brewing, BrewState::BrewSettling) => {
                info!("Brewing finished, settling");
                if let Err(e) = self.relay_controller.turn_off().await {
                    error!("Failed to turn off relay: {:?}", e);
                } else {
                    self.state_manager.set_relay_enabled(false).await;
                }
            }
            (BrewState::BrewSettling, BrewState::Idle) => {
                info!("Returned to idle state");
                self.auto_tare_controller.reset();
            }
            _ => {}
        }
    }

    async fn handle_auto_tare_action(&mut self, action: TareAction) {
        match action {
            TareAction::Tare => {
                info!("Auto-tare triggered");
                if let Err(e) = self.ble_client.send_tare_command().await {
                    warn!("Failed to send auto-tare command: {:?}", e);
                }
            }
        }
    }

    async fn handle_ble_status_change(&mut self, connected: bool) {
        self.state_manager.set_ble_connected(connected).await;

        if !connected {
            self.state_manager
                .set_error(Some("BLE disconnected".to_string()))
                .await;
        } else {
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
                if let Err(e) = self.ble_client.send_tare_command().await {
                    warn!("Failed to send tare command: {:?}", e);
                    self.state_manager
                        .add_log("Failed to tare scale".to_string())
                        .await;
                } else {
                    self.state_manager
                        .add_log("Scale tared manually".to_string())
                        .await;
                }
            }

            WebSocketCommand::StartTimer => {
                if let Err(e) = self.ble_client.send_start_timer_command().await {
                    warn!("Failed to send start timer command: {:?}", e);
                } else {
                    self.state_manager
                        .add_log("Timer started manually".to_string())
                        .await;
                }
            }

            WebSocketCommand::StopTimer => {
                if let Err(e) = self.stop_brewing().await {
                    error!("Failed to stop brewing: {:?}", e);
                }
            }

            WebSocketCommand::ResetTimer => {
                if let Err(e) = self.ble_client.send_reset_timer_command().await {
                    warn!("Failed to send reset timer command: {:?}", e);
                } else {
                    self.state_manager
                        .add_log("Timer reset manually".to_string())
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
    }

    async fn stop_brewing(&mut self) -> Result<(), RelayError> {
        info!("Stopping brewing");

        if let Err(e) = self.ble_client.send_stop_timer_command().await {
            warn!("Failed to send stop timer command: {:?}", e);
        }

        self.relay_controller.turn_off().await?;
        self.state_manager.set_relay_enabled(false).await;
        self.state_manager
            .add_log("Brewing stopped".to_string())
            .await;

        Ok(())
    }

    async fn emergency_stop(&mut self) {
        error!("EMERGENCY STOP ACTIVATED");

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
}

// Embassy task functions
#[embassy_executor::task]
async fn ble_task(ble_client: BleScaleClient) {
    info!("BLE task started");
    if let Err(e) = ble_client.start().await {
        error!("BLE task error: {:?}", e);
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
