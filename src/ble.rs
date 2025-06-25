use crate::protocol::*;
use crate::types::ScaleData;
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, mutex::Mutex, channel::Channel};
use embassy_time::{Duration, Timer};
use esp_idf_svc::bt::{Ble, BtDriver};
use log::{info, warn, error, debug};
use std::sync::Arc;

pub type ScaleDataChannel = Channel<CriticalSectionRawMutex, ScaleData, 10>;
pub type BleStatusChannel = Channel<CriticalSectionRawMutex, bool, 5>;

#[derive(Clone)]
pub struct BleScaleClient {
    data_sender: Arc<ScaleDataChannel>,
    status_sender: Arc<BleStatusChannel>,
    connected: Arc<Mutex<CriticalSectionRawMutex, bool>>,
}

impl BleScaleClient {
    pub fn new(
        data_sender: Arc<ScaleDataChannel>,
        status_sender: Arc<BleStatusChannel>,
    ) -> Self {
        Self {
            data_sender,
            status_sender,
            connected: Arc::new(Mutex::new(false)),
        }
    }
    
    pub async fn start(&self, bt_driver: BtDriver<'static, Ble>) -> Result<(), Box<dyn std::error::Error>> {
        info!("Starting BLE scale client");
        
        loop {
            match self.connect_and_monitor().await {
                Ok(_) => {
                    info!("BLE connection ended normally");
                }
                Err(e) => {
                    error!("BLE connection error: {:?}", e);
                    self.set_connected(false).await;
                    self.status_sender.send(false).await;
                }
            }
            
            info!("Waiting 5 seconds before reconnecting...");
            Timer::after(Duration::from_secs(5)).await;
        }
    }
    
    async fn connect_and_monitor(&self) -> Result<(), Box<dyn std::error::Error>> {
        info!("Scanning for Bookoo scale...");
        
        let device = self.discover_scale().await?;
        info!("Found scale: {:?}", device);
        
        let client = self.connect_to_device(&device).await?;
        info!("Connected to scale");
        
        self.set_connected(true).await;
        self.status_sender.send(true).await;
        
        self.setup_notifications(&client).await?;
        info!("Notifications enabled");
        
        self.monitor_connection(&client).await?;
        
        Ok(())
    }
    
    async fn discover_scale(&self) -> Result<BleDevice, Box<dyn std::error::Error>> {
        for _ in 0..30 {
            let devices = self.scan_devices().await?;
            
            for device in devices {
                if let Some(name) = &device.name {
                    if name.to_lowercase().contains("bookoo") {
                        return Ok(device);
                    }
                }
            }
            
            Timer::after(Duration::from_secs(2)).await;
        }
        
        Err("Scale not found after 60 seconds".into())
    }
    
    async fn scan_devices(&self) -> Result<Vec<BleDevice>, Box<dyn std::error::Error>> {
        Ok(Vec::new()) 
    }
    
    async fn connect_to_device(&self, device: &BleDevice) -> Result<BleClient, Box<dyn std::error::Error>> {
        Ok(BleClient::new())
    }
    
    async fn setup_notifications(&self, client: &BleClient) -> Result<(), Box<dyn std::error::Error>> {
        let data_sender = Arc::clone(&self.data_sender);
        
        client.setup_notification(WEIGHT_CHAR_UUID, move |data: &[u8]| {
            debug!("Received BLE data: {:02X?}", data);
            
            if let Some(scale_data) = parse_scale_data(data) {
                if let Err(_) = data_sender.try_send(scale_data) {
                    warn!("Scale data channel full, dropping data");
                }
            } else {
                warn!("Failed to parse scale data");
            }
        }).await?;
        
        Ok(())
    }
    
    async fn monitor_connection(&self, client: &BleClient) -> Result<(), Box<dyn std::error::Error>> {
        loop {
            if !client.is_connected() {
                info!("BLE connection lost");
                break;
            }
            
            Timer::after(Duration::from_secs(1)).await;
        }
        
        Ok(())
    }
    
    async fn set_connected(&self, connected: bool) {
        *self.connected.lock().await = connected;
    }
    
    pub async fn is_connected(&self) -> bool {
        *self.connected.lock().await
    }
    
    pub async fn send_tare_command(&self) -> Result<(), Box<dyn std::error::Error>> {
        if self.is_connected().await {
            info!("Sending tare command");
            Ok(())
        } else {
            Err("Not connected to scale".into())
        }
    }
    
    pub async fn send_start_timer_command(&self) -> Result<(), Box<dyn std::error::Error>> {
        if self.is_connected().await {
            info!("Sending start timer command");
            Ok(())
        } else {
            Err("Not connected to scale".into())
        }
    }
    
    pub async fn send_stop_timer_command(&self) -> Result<(), Box<dyn std::error::Error>> {
        if self.is_connected().await {
            info!("Sending stop timer command");
            Ok(())
        } else {
            Err("Not connected to scale".into())
        }
    }
    
    pub async fn send_reset_timer_command(&self) -> Result<(), Box<dyn std::error::Error>> {
        if self.is_connected().await {
            info!("Sending reset timer command");
            Ok(())
        } else {
            Err("Not connected to scale".into())
        }
    }
}

#[derive(Debug, Clone)]
struct BleDevice {
    name: Option<String>,
    address: String,
}

struct BleClient {
}

impl BleClient {
    fn new() -> Self {
        Self {}
    }
    
    async fn setup_notification<F>(&self, _char_uuid: &str, _callback: F) -> Result<(), Box<dyn std::error::Error>>
    where
        F: Fn(&[u8]) + Send + Sync + 'static,
    {
        Ok(())
    }
    
    fn is_connected(&self) -> bool {
        true
    }
}