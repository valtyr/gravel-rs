use crate::protocol::*;
use crate::types::ScaleData;
use anyhow::Error;
use bstr::ByteSlice;
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, channel::Channel, mutex::Mutex};
use embassy_time::{Duration, Timer};
use esp32_nimble::{utilities::BleUuid, BLEAdvertisedDevice, BLEClient, BLEDevice, BLEScan};
use log::{debug, error, info, warn};
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
    pub fn new(data_sender: Arc<ScaleDataChannel>, status_sender: Arc<BleStatusChannel>) -> Self {
        Self {
            data_sender,
            status_sender,
            connected: Arc::new(Mutex::new(false)),
        }
    }

    pub async fn start(&self) -> Result<(), Box<dyn std::error::Error>> {
        info!("Starting BLE scale client with NimBLE");

        // Initialize BLE device (no parameters needed)
        BLEDevice::init();

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

        let mut client = self.connect_to_device(&device).await?;

        info!("Connected to scale");

        self.set_connected(true).await;
        self.status_sender.send(true).await;

        self.setup_notifications(&mut client).await?;
        info!("Notifications enabled");

        self.monitor_connection(&client).await?;

        Ok(())
    }

    async fn discover_scale(&self) -> Result<BLEAdvertisedDevice, Box<dyn std::error::Error>> {
        for _ in 0..30 {
            info!("Starting BLE scan...");

            let ble_device = BLEDevice::take();
            let mut ble_scan = BLEScan::new();

            let scan_result = ble_scan
                .start(&ble_device, 10000, |device, data| {
                    if let Some(name) = data.name() {
                        info!("Found device: {}", name);
                        if name
                            .to_str()
                            .unwrap_or("")
                            .to_lowercase()
                            .contains("bookoo")
                        {
                            return Some(*device);
                        }
                    }
                    None
                })
                .await;

            match scan_result {
                Ok(Some(device)) => {
                    info!("Found Bookoo scale!");
                    return Ok(device);
                }
                Ok(None) => {
                    info!("No Bookoo scale found in this scan cycle");
                }
                Err(e) => {
                    warn!("BLE scan error: {:?}", e);
                }
            }

            Timer::after(Duration::from_secs(2)).await;
        }

        Err("Scale not found after 60 seconds".into())
    }

    async fn connect_to_device(
        &self,
        device: &BLEAdvertisedDevice,
    ) -> Result<BLEClient, Box<dyn std::error::Error>> {
        info!("Connecting to device: {:?}", device.addr());

        let ble_device = BLEDevice::take();
        let mut client = ble_device.new_client();
        client.connect(&device.addr()).await?;

        info!("Connected to scale");
        Ok(client)
    }

    async fn setup_notifications(
        &self,
        client: &mut BLEClient,
    ) -> Result<(), Box<dyn std::error::Error>> {
        info!("Setting up notifications for weight characteristic");

        // Get the weight service using the Bookoo service UUID
        let service = client
            .get_service(
                BleUuid::from_uuid128_string(BLE_SERVICE_UUID)
                    .map_err(|e| format!("UUID error: {}", e))?,
            )
            .await?;

        // Get the weight characteristic
        let characteristic = service
            .get_characteristic(
                BleUuid::from_uuid128_string(WEIGHT_CHAR_UUID)
                    .map_err(|e| format!("UUID error: {}", e))?,
            )
            .await?;

        let data_sender = Arc::clone(&self.data_sender);

        // Set up notification callback
        characteristic.on_notify(move |data: &[u8]| {
            debug!("Received BLE data: {:02X?}", data);

            if let Some(scale_data) = parse_scale_data(data) {
                if let Err(_) = data_sender.try_send(scale_data) {
                    warn!("Scale data channel full, dropping data");
                }
            } else {
                warn!("Failed to parse scale data");
            }
        });

        // Subscribe to notifications
        characteristic.subscribe_notify(true).await?;

        Ok(())
    }

    async fn wait_for_connection(
        &self,
        client: &BLEClient,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut i = 0;
        loop {
            i += 1;
            if client.connected() {
                info!("Connection confirmed");
                break;
            }

            if i > 20 {
                return Err(Error::msg("Failed to connect").into());
            }

            Timer::after(Duration::from_millis(100)).await;
        }

        Ok(())
    }

    async fn monitor_connection(
        &self,
        client: &BLEClient,
    ) -> Result<(), Box<dyn std::error::Error>> {
        loop {
            if !client.connected() {
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
            // TODO: Implement actual BLE command sending
            Ok(())
        } else {
            Err("Not connected to scale".into())
        }
    }

    pub async fn send_start_timer_command(&self) -> Result<(), Box<dyn std::error::Error>> {
        if self.is_connected().await {
            info!("Sending start timer command");
            // TODO: Implement actual BLE command sending
            Ok(())
        } else {
            Err("Not connected to scale".into())
        }
    }

    pub async fn send_stop_timer_command(&self) -> Result<(), Box<dyn std::error::Error>> {
        if self.is_connected().await {
            info!("Sending stop timer command");
            // TODO: Implement actual BLE command sending
            Ok(())
        } else {
            Err("Not connected to scale".into())
        }
    }

    pub async fn send_reset_timer_command(&self) -> Result<(), Box<dyn std::error::Error>> {
        if self.is_connected().await {
            info!("Sending reset timer command");
            // TODO: Implement actual BLE command sending
            Ok(())
        } else {
            Err("Not connected to scale".into())
        }
    }
}
