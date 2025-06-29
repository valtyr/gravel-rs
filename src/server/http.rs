use crate::types::SystemState;
use anyhow;
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, channel::Channel, mutex::Mutex};
use embassy_time::{Duration, Timer};
use esp_idf_svc::http::server::{Configuration, EspHttpServer};
use esp_idf_svc::http::Method;
use esp_idf_svc::io::Write;
use log::{debug, error, info, warn};
use serde::{Deserialize, Serialize};
use serde_json;
use std::sync::Arc;

pub type WebSocketCommandChannel = Channel<CriticalSectionRawMutex, WebSocketCommand, 10>;

// Note: WebSocket connection tracking removed - using simple HTTP polling now

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

        // Using individual connection broadcasting for ESP-IDF compatibility
        info!("WebSocket server starting with individual connection management");

        // Configure HTTP server with much higher session limits for WebSocket
        let config = Configuration {
            stack_size: 10240, // Larger stack for WebSocket threads
            session_timeout: std::time::Duration::from_secs(300), // 5 minute timeout for WebSocket
            max_sessions: 16,  // Match ESP-IDF config - plenty for WebSocket + HTTP requests
            ..Default::default()
        };
        let mut server = EspHttpServer::new(&config)?;

        // Serve the main HTML page
        server.fn_handler("/", Method::Get, |request| -> Result<(), anyhow::Error> {
            debug!("Serving main page");
            let html = include_str!("../../web/index.html");
            let mut response = request.into_response(
                200,
                Some("OK"),
                &[("Content-Type", "text/html"), ("Cache-Control", "no-cache")],
            )?;
            response.write_all(html.as_bytes())?;
            debug!("Main page served successfully");
            Ok(())
        })?;

        // Serve CSS
        server.fn_handler(
            "/style.css",
            Method::Get,
            |request| -> Result<(), anyhow::Error> {
                let css = include_str!("../../web/style.css");
                let mut response = request.into_response(
                    200,
                    Some("OK"),
                    &[("Content-Type", "text/css"), ("Cache-Control", "no-cache")],
                )?;
                response.write_all(css.as_bytes())?;
                Ok(())
            },
        )?;

        // Serve JavaScript
        server.fn_handler(
            "/script.js",
            Method::Get,
            |request| -> Result<(), anyhow::Error> {
                let js = include_str!("../../web/script.js");
                let mut response = request.into_response(
                    200,
                    Some("OK"),
                    &[
                        ("Content-Type", "application/javascript"),
                        ("Cache-Control", "no-cache"),
                    ],
                )?;
                response.write_all(js.as_bytes())?;
                Ok(())
            },
        )?;

        // Command endpoint for WebSocket commands sent via HTTP POST
        let command_channel_http = Arc::clone(&self.command_sender);
        server.fn_handler(
            "/command",
            Method::Post,
            move |mut request| -> Result<(), anyhow::Error> {
                info!("Received POST /command request");

                // Read request body with limited size to prevent hanging
                let mut body = Vec::new();
                let mut buffer = [0u8; 512]; // Smaller buffer for safety
                let mut total_read = 0;

                loop {
                    if total_read >= 2048 {
                        // Limit total read to 2KB
                        warn!("Request body too large, truncating");
                        break;
                    }

                    match request.read(&mut buffer) {
                        Ok(0) => break, // End of data
                        Ok(n) => {
                            body.extend_from_slice(&buffer[..n]);
                            total_read += n;
                        }
                        Err(e) => {
                            warn!("Error reading request body: {:?}", e);
                            break;
                        }
                    }
                }

                let body_str = match String::from_utf8(body) {
                    Ok(s) => s,
                    Err(e) => {
                        warn!("Invalid UTF-8 in request body: {}", e);
                        let mut response = request.into_response(400, Some("Bad Request"), &[])?;
                        response.write_all(b"Invalid UTF-8")?;
                        return Ok(());
                    }
                };

                info!("Command body: {}", body_str.trim());

                match serde_json::from_str::<WebSocketCommand>(&body_str) {
                    Ok(command) => {
                        info!("Parsed command: {:?}", command);
                        // Send command to processing channel (async, non-blocking)
                        if let Err(_) = command_channel_http.try_send(command) {
                            warn!("Command channel full, dropping command");
                        }

                        // Send successful response
                        let mut response = request.into_response(
                            200,
                            Some("OK"),
                            &[
                                ("Content-Type", "text/plain"),
                                ("Access-Control-Allow-Origin", "*"),
                            ],
                        )?;
                        response.write_all(b"Command received")?;
                        Ok(())
                    }
                    Err(e) => {
                        warn!("Failed to parse command JSON: {}", e);
                        let mut response = request.into_response(
                            400,
                            Some("Bad Request"),
                            &[
                                ("Content-Type", "text/plain"),
                                ("Access-Control-Allow-Origin", "*"),
                            ],
                        )?;
                        response.write_all(format!("Invalid JSON: {}", e).as_bytes())?;
                        Ok(())
                    }
                }
            },
        )?;

        // State endpoint for client polling (replaces WebSocket)
        let state_handle = Arc::clone(&self.state);
        server.fn_handler(
            "/state",
            Method::Get,
            move |request| -> Result<(), anyhow::Error> {
                debug!("Serving /state endpoint for polling client");

                if let Ok(state) = state_handle.try_lock() {
                    let response = WebSocketResponse {
                        scale_data: state.scale_data.as_ref().map(|data| ScaleDataMsg {
                            weight_g: data.weight_g,
                            flow_rate_g_per_s: data.flow_rate_g_per_s,
                            battery_percent: data.battery_percent,
                            timer_running: data.timer_running,
                            timestamp_ms: data.timestamp_ms,
                        }),
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
                        let mut http_response = request.into_response(
                            200,
                            Some("OK"),
                            &[
                                ("Content-Type", "application/json"),
                                ("Cache-Control", "no-cache"),
                                ("Access-Control-Allow-Origin", "*"),
                            ],
                        )?;
                        http_response.write_all(json.as_bytes())?;
                        debug!("Successfully served state JSON ({} bytes)", json.len());
                    } else {
                        warn!("Failed to serialize state to JSON");
                        let mut http_response =
                            request.into_response(500, Some("Internal Server Error"), &[])?;
                        http_response.write_all(b"Failed to serialize state")?;
                    }
                } else {
                    warn!("State locked, returning error");
                    let mut http_response =
                        request.into_response(503, Some("Service Unavailable"), &[])?;
                    http_response.write_all(b"State temporarily unavailable")?;
                }

                Ok(())
            },
        )?;

        info!("HTTP server started successfully (polling mode)");
        info!("Server configuration:");
        info!("  Max sessions: {}", config.max_sessions);
        info!("  Session timeout: {:?}", config.session_timeout);
        info!("  Stack size: {}", config.stack_size);
        info!("Available endpoints:");
        info!("  GET  / - Web interface");
        info!("  GET  /style.css - Stylesheet");
        info!("  GET  /script.js - JavaScript");
        info!("  GET  /state - Real-time state (for 5Hz polling)");
        info!("  POST /command - Command endpoint");

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
