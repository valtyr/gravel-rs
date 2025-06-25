use crate::types::SystemState;
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, mutex::Mutex, channel::Channel};
use embassy_time::{Duration, Timer};
use esp_idf_svc::http::server::{EspHttpServer, Configuration};
use esp_idf_svc::http::Method;
use esp_idf_svc::io::Write;
use log::{info, debug};
use std::sync::Arc;

pub type WebSocketCommandChannel = Channel<CriticalSectionRawMutex, WebSocketCommand, 10>;

#[derive(Debug, Clone)]
pub enum WebSocketCommand {
    SetTargetWeight(f32),
    SetAutoTare(bool),
    SetPredictiveStop(bool),
    TareScale,
    StartTimer,
    StopTimer,
    ResetTimer,
    ResetOvershoot,
    TestRelay,
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
        
        info!("HTTP server started successfully");
        
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
        WebSocketCommand::SetTargetWeight(weight) => {
            info!("Would set target weight to: {:.1}g", weight);
        }
        WebSocketCommand::SetAutoTare(enabled) => {
            info!("Would set auto-tare to: {}", enabled);
        }
        WebSocketCommand::SetPredictiveStop(enabled) => {
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