use crate::protocol::*;
use crate::types::ScaleData;
use crate::ble_bindings::CustomBleClient;
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, channel::Channel};
use log::{info, warn, error};
use std::sync::Arc;

pub type ScaleDataChannel = Channel<CriticalSectionRawMutex, ScaleData, 10>;
pub type BleStatusChannel = Channel<CriticalSectionRawMutex, bool, 5>;

#[derive(Clone)]
pub struct BleScaleClient {
    custom_client: Arc<CustomBleClient>,
}

impl BleScaleClient {
    pub fn new(data_sender: Arc<ScaleDataChannel>, status_sender: Arc<BleStatusChannel>) -> Self {
        let custom_client = CustomBleClient::new(data_sender, status_sender);
        Self {
            custom_client: Arc::new(custom_client),
        }
    }

    pub async fn start(&self) -> Result<(), Box<dyn std::error::Error>> {
        info!("Starting BLE scale client with custom NimBLE implementation");
        self.custom_client.start().await
    }

    pub async fn is_connected(&self) -> bool {
        self.custom_client.is_connected().await
    }

    pub async fn send_tare_command(&self) -> Result<(), Box<dyn std::error::Error>> {
        self.custom_client.send_tare_command().await
    }

    pub async fn send_start_timer_command(&self) -> Result<(), Box<dyn std::error::Error>> {
        self.custom_client.send_start_timer_command().await
    }

    pub async fn send_stop_timer_command(&self) -> Result<(), Box<dyn std::error::Error>> {
        self.custom_client.send_stop_timer_command().await
    }

    pub async fn send_reset_timer_command(&self) -> Result<(), Box<dyn std::error::Error>> {
        self.custom_client.send_reset_timer_command().await
    }
}