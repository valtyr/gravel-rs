use crate::types::SystemState;
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, mutex::Mutex, channel::Channel};
use embassy_time::{Duration, Timer};
use esp_idf_svc::http::server::{EspHttpServer, Configuration};
use esp_idf_svc::http::Method;
use esp_idf_svc::io::Write;
use esp_idf_svc::ws::FrameType;
use log::{info, debug, warn, error};
use serde::{Serialize, Deserialize};
use serde_json;
use std::sync::Arc;
use std::sync::mpsc;
use std::thread;
use anyhow;

pub type WebSocketCommandChannel = Channel<CriticalSectionRawMutex, WebSocketCommand, 10>;

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum WebSocketCommand {
    #[serde(rename = "set_target_weight")]
    SetTargetWeight { weight: f32 },
    #[serde(rename = "set_auto_tare")]
    SetAutoTare { enabled: bool },
    #[serde(rename = "set_predictive_stop")]
    SetPredictiveStop { enabled: bool },
    #[serde(rename = "tare_scale")]
    TareScale,
    #[serde(rename = "start_timer")]
    StartTimer,
    #[serde(rename = "stop_timer")]
    StopTimer,
    #[serde(rename = "reset_timer")]
    ResetTimer,
    #[serde(rename = "reset_overshoot")]
    ResetOvershoot,
    #[serde(rename = "test_relay")]
    TestRelay,
}

#[derive(Debug, Serialize)]
pub struct WebSocketResponse {
    pub scale_data: Option<ScaleDataMsg>,
    pub system_state: SystemStateMsg,
    pub timestamp: u64,
}

#[derive(Debug, Serialize)]
pub struct ScaleDataMsg {
    pub weight_g: f32,
    pub flow_rate_g_per_s: f32,
    pub battery_percent: u8,
    pub timer_running: bool,
    pub timestamp_ms: u32,
}

#[derive(Debug, Serialize)]
pub struct SystemStateMsg {
    pub brew_state: String,
    pub timer_state: String,
    pub target_weight_g: f32,
    pub auto_tare_enabled: bool,
    pub predictive_stop_enabled: bool,
    pub relay_enabled: bool,
    pub ble_connected: bool,
    pub error: Option<String>,
    pub overshoot_info: String,
}

#[derive(Clone)]
pub struct WebSocketServer {
    state: Arc<Mutex<CriticalSectionRawMutex, SystemState>>,
    command_sender: Arc<WebSocketCommandChannel>,
}

impl WebSocketServer {
    pub fn new(
        state: Arc<Mutex<CriticalSectionRawMutex, SystemState>>,
        command_sender: Arc<WebSocketCommandChannel>,
        _port: u16,
    ) -> Self {
        Self {
            state,
            command_sender,
        }
    }
    
    pub async fn start(&self) -> Result<(), Box<dyn std::error::Error>> {
        info!("Starting HTTP server with WebSocket support");
        
        let config = Configuration::default();
        let mut server = EspHttpServer::new(&config)?;
        
        // Serve the main HTML page
        server.fn_handler("/", Method::Get, |request| -> Result<(), anyhow::Error> {
            let html = include_str!("../web/index.html");
            let mut response = request.into_ok_response()?;
            response.write_all(html.as_bytes())?;
            Ok(())
        })?;
        
        // Serve CSS
        server.fn_handler("/style.css", Method::Get, |request| -> Result<(), anyhow::Error> {
            let css = include_str!("../web/style.css");
            let mut response = request.into_response(200, Some("OK"), &[("Content-Type", "text/css")])?;
            response.write_all(css.as_bytes())?;
            Ok(())
        })?;
        
        // Serve JavaScript
        server.fn_handler("/script.js", Method::Get, |request| -> Result<(), anyhow::Error> {
            let js = include_str!("../web/script.js");
            let mut response = request.into_response(200, Some("OK"), &[("Content-Type", "application/javascript")])?;
            response.write_all(js.as_bytes())?;
            Ok(())
        })?;

        // WebSocket endpoint for real-time data and commands
        let state_handle = Arc::clone(&self.state);
        let command_channel = Arc::clone(&self.command_sender);
        
        server.ws_handler("/ws", move |connection| {
            info!("New WebSocket connection established");
            
            // Send welcome message
            if let Err(e) = connection.send(FrameType::Text(false), b"Connected to Espresso Scale Controller") {
                warn!("Failed to send welcome message: {:?}", e);
                return anyhow::Ok(());
            }

            // Create a channel for this WebSocket connection to receive state updates
            let (state_sender, state_receiver) = mpsc::sync_channel::<String>(100);
            
            // Clone handles for the sender thread
            let state_handle_clone = Arc::clone(&state_handle);
            let command_channel_clone = Arc::clone(&command_channel);
            
            // Spawn thread to periodically send state updates
            let sender_thread = {
                let state_sender = state_sender.clone();
                thread::spawn(move || {
                    loop {
                        // Send current state every 100ms
                        if let Ok(state) = state_handle_clone.try_lock() {
                            let response = WebSocketResponse {
                                scale_data: None, // Will be populated when we have real scale data
                                system_state: SystemStateMsg {
                                    brew_state: format!("{:?}", state.brew_state),
                                    timer_state: format!("{:?}", state.timer_state),
                                    target_weight_g: state.config.target_weight_g,
                                    auto_tare_enabled: state.config.auto_tare,
                                    predictive_stop_enabled: state.config.predictive_stop,
                                    relay_enabled: state.relay_enabled,
                                    ble_connected: state.ble_connected,
                                    error: state.last_error.clone(),
                                    overshoot_info: "Learning data not available".to_string(),
                                },
                                timestamp: std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_secs(),
                            };
                            
                            if let Ok(json) = serde_json::to_string(&response) {
                                if state_sender.send(json).is_err() {
                                    break; // Connection closed
                                }
                            }
                        }
                        
                        thread::sleep(std::time::Duration::from_millis(100));
                    }
                })
            };

            // Main WebSocket loop - handle incoming messages and send outgoing data
            loop {
                if connection.is_closed() {
                    info!("WebSocket connection closed");
                    break;
                }

                // Check for incoming commands (non-blocking)
                // Note: ESP-IDF WebSocket doesn't have built-in message receiving in this handler
                // For now, focus on sending real-time data
                
                // Send state updates to client
                match state_receiver.recv_timeout(std::time::Duration::from_millis(50)) {
                    Ok(state_json) => {
                        if let Err(e) = connection.send(FrameType::Text(false), state_json.as_bytes()) {
                            warn!("Failed to send state update: {:?}", e);
                            break;
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        // No state update, send heartbeat
                        if let Err(e) = connection.send(FrameType::Text(false), b"heartbeat") {
                            warn!("Failed to send heartbeat: {:?}", e);
                            break;
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        info!("State update channel disconnected");
                        break;
                    }
                }
            }

            // Clean up sender thread
            drop(state_sender);
            if let Err(e) = sender_thread.join() {
                warn!("Error joining sender thread: {:?}", e);
            }

            info!("WebSocket handler completed");
            anyhow::Ok(())
        })?;
        
        info!("HTTP server with WebSocket started successfully");
        info!("Available endpoints:");
        info!("  GET / - Web interface");
        info!("  GET /style.css - Stylesheet");  
        info!("  GET /script.js - JavaScript");
        info!("  WS  /ws - WebSocket real-time data");
        
        // Keep server alive
        loop {
            Timer::after(Duration::from_secs(10)).await;
            debug!("HTTP server heartbeat");
        }
    }
    
    pub async fn serve_http(&self) -> Result<(), Box<dyn std::error::Error>> {
        // This is now combined with start() method
        self.start().await
    }
}

// Helper function for processing WebSocket commands (simplified for build)
pub async fn process_websocket_command(
    command: WebSocketCommand,
    state: &Arc<Mutex<CriticalSectionRawMutex, SystemState>>,
) -> Result<(), Box<dyn std::error::Error>> {
    debug!("Processing WebSocket command: {:?}", command);
    
    // In a full implementation, this would update the system state
    // based on the received command
    match command {
        WebSocketCommand::SetTargetWeight { weight } => {
            info!("Would set target weight to: {:.1}g", weight);
        }
        WebSocketCommand::SetAutoTare { enabled } => {
            info!("Would set auto-tare to: {}", enabled);
        }
        WebSocketCommand::SetPredictiveStop { enabled } => {
            info!("Would set predictive stop to: {}", enabled);
        }
        WebSocketCommand::TareScale => {
            info!("Would send tare command");
        }
        WebSocketCommand::StartTimer => {
            info!("Would start timer");
        }
        WebSocketCommand::StopTimer => {
            info!("Would stop timer");
        }
        WebSocketCommand::ResetTimer => {
            info!("Would reset timer");
        }
        WebSocketCommand::ResetOvershoot => {
            info!("Would reset overshoot learning");
        }
        WebSocketCommand::TestRelay => {
            info!("Would test relay");
        }
    }
    
    Ok(())
}