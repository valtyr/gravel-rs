// bookoo_scale.rs - Bookoo Themis Mini scale implementation using generic BLE client
// This module provides high-level interface for the Bookoo scale using the generic BLE client

use crate::ble::{
    BleClient, BleError, Characteristic, Connection, Device, DeviceFilter, StatusChannel, Uuid,
};
use crate::scales::protocol::parse_scale_data;
use crate::scales::traits::{
    BleScale, ScaleCapabilities, ScaleCommand, ScaleCommandChannel, ScaleDataChannel, ScaleInfo,
    SmartScale,
};
use crate::types::ScaleData;
use embassy_time::{Duration, Timer};
use log::{debug, error, info, warn};
use std::sync::Arc;

// Bookoo scale UUIDs - scale uses 16-bit UUIDs, not 128-bit
const BOOKOO_SERVICE_UUID_16: u16 = 0x0FFE; // Service UUID as 16-bit (discovered from hardware)
const WEIGHT_CHAR_UUID_16: u16 = 0xFF11; // Weight characteristic UUID as 16-bit
const COMMAND_CHAR_UUID_16: u16 = 0xFF12; // Command characteristic UUID as 16-bit

// Fallback 128-bit UUIDs (in case some scales use full UUIDs)
const BOOKOO_SERVICE_UUID_128: [u8; 16] = [
    0xfb, 0x34, 0x9b, 0x5f, 0x80, 0x00, 0x00, 0x80, 0x00, 0x10, 0x00, 0x00, 0xe0, 0xff, 0x00, 0x00,
]; // 0000ffe0-0000-1000-8000-00805f9b34fb

const WEIGHT_CHAR_UUID_128: [u8; 16] = [
    0xfb, 0x34, 0x9b, 0x5f, 0x80, 0x00, 0x00, 0x80, 0x00, 0x10, 0x00, 0x00, 0x11, 0xff, 0x00, 0x00,
]; // 0000ff11-0000-1000-8000-00805f9b34fb

const COMMAND_CHAR_UUID_128: [u8; 16] = [
    0xfb, 0x34, 0x9b, 0x5f, 0x80, 0x00, 0x00, 0x80, 0x00, 0x10, 0x00, 0x00, 0x12, 0xff, 0x00, 0x00,
]; // 0000ff12-0000-1000-8000-00805f9b34fb

// Scale error types
#[derive(Debug)]
pub enum ScaleError {
    BleError(BleError),
    ScaleNotFound,
    ServiceNotFound,
    CharacteristicNotFound,
    NotConnected,
    CommandFailed(String),
}

impl std::fmt::Display for ScaleError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            ScaleError::BleError(e) => write!(f, "BLE error: {}", e),
            ScaleError::ScaleNotFound => write!(f, "Bookoo scale not found"),
            ScaleError::ServiceNotFound => write!(f, "Scale service not found"),
            ScaleError::CharacteristicNotFound => write!(f, "Scale characteristic not found"),
            ScaleError::NotConnected => write!(f, "Not connected to scale"),
            ScaleError::CommandFailed(msg) => write!(f, "Command failed: {}", msg),
        }
    }
}

impl std::error::Error for ScaleError {}

impl From<BleError> for ScaleError {
    fn from(error: BleError) -> Self {
        ScaleError::BleError(error)
    }
}

// Bookoo scale client
pub struct BookooScale {
    ble_client: BleClient,
    data_channel: Arc<ScaleDataChannel>,
    connection: Option<Connection>,
    weight_characteristic: Option<Characteristic>,
    command_characteristic: Option<Characteristic>,
    info: ScaleInfo,
}

impl BookooScale {
    pub fn new(data_channel: Arc<ScaleDataChannel>, status_channel: Arc<StatusChannel>) -> Self {
        let ble_client = BleClient::new(status_channel);

        let info = ScaleInfo {
            brand: "Bookoo".to_string(),
            model: "Themis Mini".to_string(),
            version: None,
            capabilities: ScaleCapabilities {
                has_timer: true,
                has_flow_rate: true,
                has_battery_level: true,
                supports_tare: true,
                supports_auto_off: false,
            },
        };

        Self {
            ble_client,
            data_channel,
            connection: None,
            weight_characteristic: None,
            command_characteristic: None,
            info,
        }
    }

    /// Initialize the BLE stack (call once at startup)
    pub fn initialize() -> Result<(), ScaleError> {
        BleClient::initialize().map_err(ScaleError::from)
    }

    /// Reset the BLE stack (for use after WiFi provisioning)
    pub fn reset_ble_stack() -> Result<(), ScaleError> {
        info!("🔄 Resetting BLE stack after WiFi provisioning");

        // WiFi provisioning already cleaned up BLE - we just need to reinitialize
        unsafe {
            use esp_idf_svc::sys::*;

            // Wait for any pending operations to complete
            embassy_time::block_for(embassy_time::Duration::from_millis(1000));

            // Check if NimBLE is already stopped by provisioning
            // If so, we just need to reinitialize
            let ret = nimble_port_init();
            if ret != ESP_OK {
                warn!("NimBLE init failed: {}, trying full reset", ret);

                // Try a gentle reset - deinit first
                nimble_port_deinit();
                embassy_time::block_for(embassy_time::Duration::from_millis(500));

                // Then reinitialize
                let ret = nimble_port_init();
                if ret != ESP_OK {
                    return Err(ScaleError::BleError(BleError::InitializationFailed(
                        format!("NimBLE reinit failed: {}", ret),
                    )));
                }
            }

            // Start NimBLE host task
            nimble_port_freertos_init(None);
        }

        info!("✅ BLE stack reset complete");
        Ok(())
    }

    /// Start the scale client - scan, connect, and monitor
    pub async fn start(&mut self) -> Result<(), ScaleError> {
        info!("Starting Bookoo scale client");

        loop {
            match self.connect_and_monitor().await {
                Ok(_) => {
                    info!("Scale connection cycle completed");
                }
                Err(e) => {
                    error!("Scale connection error: {:?}", e);
                    self.cleanup_connection().await;
                }
            }

            info!("Waiting 5 seconds before retrying scale connection...");
            Timer::after(Duration::from_secs(5)).await;
        }
    }

    /// Start the scale client with command channel support
    pub async fn start_with_commands(
        &mut self,
        command_channel: Arc<ScaleCommandChannel>,
    ) -> Result<(), ScaleError> {
        info!("Starting Bookoo scale client with command channel");

        loop {
            match self
                .connect_and_monitor_with_commands(command_channel.clone())
                .await
            {
                Ok(_) => {
                    info!("Scale connection cycle completed");
                }
                Err(e) => {
                    error!("Scale connection error: {:?}", e);
                    self.cleanup_connection().await;
                }
            }

            info!("Waiting 5 seconds before retrying scale connection...");
            Timer::after(Duration::from_secs(5)).await;
        }
    }

    /// Connect to scale and monitor for data
    async fn connect_and_monitor(&mut self) -> Result<(), ScaleError> {
        // Step 1: Scan for Bookoo scale
        let scale_device = self.find_scale().await?;
        info!("Found Bookoo scale: {:?}", scale_device.name);

        // Step 2: Connect to the scale
        let connection = self.ble_client.connect(&scale_device).await?;
        self.connection = Some(connection.clone());
        info!("Connected to Bookoo scale");

        // Step 3: Discover services and characteristics
        self.discover_scale_services(&connection).await?;
        info!("Discovered scale services and characteristics");

        // Step 4: Subscribe to weight notifications with retry logic
        if let Some(ref weight_char) = self.weight_characteristic {
            self.subscribe_to_notifications_resilient(&connection, weight_char)
                .await?;
            info!("Subscribed to weight notifications");
        } else {
            return Err(ScaleError::CharacteristicNotFound);
        }

        // Step 5: Monitor for data
        self.monitor_scale_data().await?;

        Ok(())
    }

    /// Connect to scale and monitor for data with command processing
    async fn connect_and_monitor_with_commands(
        &mut self,
        command_channel: Arc<ScaleCommandChannel>,
    ) -> Result<(), ScaleError> {
        // Step 1: Scan for Bookoo scale
        let scale_device = self.find_scale().await?;
        info!("Found Bookoo scale: {:?}", scale_device.name);

        // Step 2: Connect to the scale
        let connection = self.ble_client.connect(&scale_device).await?;
        self.connection = Some(connection.clone());
        info!("Connected to Bookoo scale");

        // Step 3: Discover services and characteristics
        self.discover_scale_services(&connection).await?;
        info!("Discovered scale services and characteristics");

        // Step 4: Subscribe to weight notifications with retry logic
        if let Some(ref weight_char) = self.weight_characteristic {
            self.subscribe_to_notifications_resilient(&connection, weight_char)
                .await?;
            info!("Subscribed to weight notifications");
        } else {
            return Err(ScaleError::CharacteristicNotFound);
        }

        // Step 5: Monitor for data and commands
        self.monitor_scale_data_with_commands(command_channel)
            .await?;

        Ok(())
    }

    /// Scan for Bookoo scale devices - connect immediately when found
    async fn find_scale(&self) -> Result<Device, ScaleError> {
        info!("Scanning for Bookoo scale...");

        let filter = DeviceFilter {
            name_prefix: Some("BOOKOO_SC".to_string()),
            service_uuid: None,
        };

        // Use early termination scan to connect immediately when scale is found
        if let Some(device) = self
            .ble_client
            .scan_for_first_device(Some(filter), 10000)
            .await?
        {
            if let Some(ref name) = device.name {
                if name.starts_with("BOOKOO_SC") {
                    info!("Found Bookoo scale immediately: {}", name);
                    return Ok(device);
                }
            }
        }

        Err(ScaleError::ScaleNotFound)
    }

    /// Connect directly to a specific device without scanning
    /// This method is used by the generic scanner to connect to a pre-discovered device
    pub async fn connect_to_device(&mut self, device: Device) -> Result<(), ScaleError> {
        info!("🔗 Connecting directly to Bookoo device: {:?}", device.name);

        // Step 1: Connect to the provided device (skip scanning)
        let connection = self.ble_client.connect(&device).await?;
        self.connection = Some(connection.clone());
        info!("✅ Connected to Bookoo device");

        // Step 2: Discover services and characteristics
        self.discover_scale_services(&connection).await?;
        info!("🔍 Discovered Bookoo scale services and characteristics");

        // Step 3: Subscribe to weight notifications with retry logic
        if let Some(ref weight_char) = self.weight_characteristic {
            self.subscribe_to_notifications_resilient(&connection, weight_char)
                .await?;
            info!("📊 Subscribed to Bookoo weight notifications");
        } else {
            return Err(ScaleError::CharacteristicNotFound);
        }

        info!("🎉 Bookoo scale connection setup complete!");
        Ok(())
    }

    /// Start monitoring connected device for scale data with command support
    /// This method is used after connect_to_device() to begin data processing
    pub async fn start_monitoring_with_commands(
        &self,
        command_channel: Arc<ScaleCommandChannel>,
    ) -> Result<(), ScaleError> {
        if !self.is_connected() {
            return Err(ScaleError::NotConnected);
        }
        
        info!("🏃 Starting Bookoo scale monitoring with commands...");
        self.monitor_scale_data_with_commands(command_channel).await
    }

    /// Start monitoring connected device for scale data (no commands)
    pub async fn start_monitoring(&self) -> Result<(), ScaleError> {
        if !self.is_connected() {
            return Err(ScaleError::NotConnected);
        }
        
        info!("🏃 Starting Bookoo scale monitoring...");
        self.monitor_scale_data().await
    }

    /// Disconnect from the scale and clean up resources
    pub async fn disconnect(&mut self) -> Result<(), ScaleError> {
        if let Some(connection) = &self.connection {
            if let Err(e) = self.ble_client.disconnect(connection).await {
                warn!("Failed to disconnect cleanly: {}", e);
                // Continue with cleanup even if disconnect fails
            }
        }

        self.connection = None;
        self.weight_characteristic = None;
        self.command_characteristic = None;

        info!("📱 Bookoo scale disconnected and cleaned up");
        Ok(())
    }

    /// Discover Bookoo scale services and characteristics (with retries for resilience)
    async fn discover_scale_services(&mut self, connection: &Connection) -> Result<(), ScaleError> {
        info!("Discovering Bookoo scale services...");

        const MAX_DISCOVERY_RETRIES: u32 = 3;
        const DISCOVERY_RETRY_DELAY_MS: u64 = 1000;

        let mut services = None;
        
        // Retry service discovery - this is often flaky on BLE connections
        for attempt in 1..=MAX_DISCOVERY_RETRIES {
            match self.ble_client.discover_services(connection).await {
                Ok(discovered_services) => {
                    info!("Service discovery succeeded on attempt {}", attempt);
                    services = Some(discovered_services);
                    break;
                }
                Err(e) => {
                    warn!("Service discovery failed on attempt {}/{}: {:?}", attempt, MAX_DISCOVERY_RETRIES, e);
                    if attempt < MAX_DISCOVERY_RETRIES {
                        info!("Retrying service discovery in {}ms...", DISCOVERY_RETRY_DELAY_MS);
                        Timer::after(Duration::from_millis(DISCOVERY_RETRY_DELAY_MS)).await;
                    } else {
                        error!("Service discovery failed after {} attempts", MAX_DISCOVERY_RETRIES);
                        return Err(ScaleError::BleError(e));
                    }
                }
            }
        }

        let services = services.ok_or(ScaleError::ServiceNotFound)?;

        // Find the Bookoo scale service (try 16-bit first, then 128-bit fallback)
        let bookoo_service_uuid_16 = Uuid::from_u16(BOOKOO_SERVICE_UUID_16);
        let bookoo_service_uuid_128 = Uuid::from_u128_bytes(BOOKOO_SERVICE_UUID_128);

        let scale_service = services
            .iter()
            .find(|service| {
                service.uuid == bookoo_service_uuid_16 || service.uuid == bookoo_service_uuid_128
            })
            .ok_or(ScaleError::ServiceNotFound)?;

        info!("Found Bookoo scale service: {:?}", scale_service);

        // Discover characteristics for the scale service (with retries)
        let mut characteristics = None;
        
        for attempt in 1..=MAX_DISCOVERY_RETRIES {
            match self
                .ble_client
                .discover_characteristics(connection, scale_service)
                .await
            {
                Ok(discovered_chars) => {
                    info!("Characteristic discovery succeeded on attempt {}", attempt);
                    characteristics = Some(discovered_chars);
                    break;
                }
                Err(e) => {
                    warn!("Characteristic discovery failed on attempt {}/{}: {:?}", attempt, MAX_DISCOVERY_RETRIES, e);
                    if attempt < MAX_DISCOVERY_RETRIES {
                        info!("Retrying characteristic discovery in {}ms...", DISCOVERY_RETRY_DELAY_MS);
                        Timer::after(Duration::from_millis(DISCOVERY_RETRY_DELAY_MS)).await;
                    } else {
                        error!("Characteristic discovery failed after {} attempts", MAX_DISCOVERY_RETRIES);
                        return Err(ScaleError::BleError(e));
                    }
                }
            }
        }

        let characteristics = characteristics.ok_or(ScaleError::CharacteristicNotFound)?;

        // Find weight and command characteristics (try both 16-bit and 128-bit UUIDs)
        let weight_uuid_16 = Uuid::from_u16(WEIGHT_CHAR_UUID_16);
        let weight_uuid_128 = Uuid::from_u128_bytes(WEIGHT_CHAR_UUID_128);
        let command_uuid_16 = Uuid::from_u16(COMMAND_CHAR_UUID_16);
        let command_uuid_128 = Uuid::from_u128_bytes(COMMAND_CHAR_UUID_128);

        for characteristic in characteristics {
            if characteristic.uuid == weight_uuid_16 || characteristic.uuid == weight_uuid_128 {
                info!(
                    "Found weight characteristic: {:?} at handle {}",
                    characteristic.uuid, characteristic.handle
                );
                self.weight_characteristic = Some(characteristic);
            } else if characteristic.uuid == command_uuid_16
                || characteristic.uuid == command_uuid_128
            {
                info!(
                    "Found command characteristic: {:?} at handle {}",
                    characteristic.uuid, characteristic.handle
                );
                self.command_characteristic = Some(characteristic);
            }
        }

        if self.weight_characteristic.is_none() {
            error!("Weight characteristic not found (tried both 0xFF11 and 0000ff11-...)");
            return Err(ScaleError::CharacteristicNotFound);
        }

        if self.command_characteristic.is_none() {
            warn!("Command characteristic not found (tried both 0xFF12 and 0000ff12-...) - commands will not work");
        }

        Ok(())
    }

    /// Subscribe to notifications with retry logic for resilience
    async fn subscribe_to_notifications_resilient(
        &self,
        connection: &Connection,
        characteristic: &Characteristic,
    ) -> Result<(), ScaleError> {
        const MAX_SUBSCRIPTION_RETRIES: u32 = 3;
        const SUBSCRIPTION_RETRY_DELAY_MS: u64 = 500;

        for attempt in 1..=MAX_SUBSCRIPTION_RETRIES {
            match self
                .ble_client
                .subscribe_to_notifications(connection, characteristic)
                .await
            {
                Ok(_) => {
                    info!("Notification subscription succeeded on attempt {}", attempt);
                    return Ok(());
                }
                Err(e) => {
                    warn!("Notification subscription failed on attempt {}/{}: {:?}", attempt, MAX_SUBSCRIPTION_RETRIES, e);
                    if attempt < MAX_SUBSCRIPTION_RETRIES {
                        info!("Retrying subscription in {}ms...", SUBSCRIPTION_RETRY_DELAY_MS);
                        Timer::after(Duration::from_millis(SUBSCRIPTION_RETRY_DELAY_MS)).await;
                    } else {
                        error!("Notification subscription failed after {} attempts", MAX_SUBSCRIPTION_RETRIES);
                        return Err(ScaleError::BleError(e));
                    }
                }
            }
        }

        unreachable!()
    }

    /// Monitor scale for incoming data
    async fn monitor_scale_data(&self) -> Result<(), ScaleError> {
        info!("Monitoring scale for weight data...");

        let mut no_data_count = 0;
        const MAX_NO_DATA_COUNT: u32 = 300; // 5 minutes without data

        loop {
            Timer::after(Duration::from_millis(100)).await;

            // Check for new notification data
            if let Some(data) = self.ble_client.get_notification_data() {
                no_data_count = 0;

                debug!("Received scale data: {} bytes: {:02X?}", data.len(), data);

                // Parse the scale data
                if let Some(scale_data) = parse_scale_data(&data) {
                    info!(
                        "Parsed weight: {:.2}g, flow: {:.2}g/s, battery: {}%, timer: {}",
                        scale_data.weight_g,
                        scale_data.flow_rate_g_per_s,
                        scale_data.battery_percent,
                        scale_data.timer_running
                    );

                    // Send data to the main application
                    if let Err(_) = self.data_channel.try_send(scale_data) {
                        warn!("Failed to send scale data - channel full");
                    }
                } else {
                    warn!(
                        "Failed to parse scale data: {} bytes: {:02X?}",
                        data.len(),
                        data
                    );
                }
            } else {
                no_data_count += 1;

                // Log status every 30 seconds
                if no_data_count % 300 == 0 {
                    info!(
                        "Waiting for scale data... ({} seconds without data)",
                        no_data_count / 10
                    );
                }

                // Timeout after 5 minutes without data
                if no_data_count > MAX_NO_DATA_COUNT {
                    warn!("No data received from scale for 5 minutes - reconnecting");
                    return Err(ScaleError::BleError(BleError::ConnectionFailed(
                        "No data timeout".to_string(),
                    )));
                }
            }

            // Check if still connected
            if self.connection.is_none() {
                return Err(ScaleError::NotConnected);
            }

            // Check BLE connection status from the client
            if !self.ble_client.is_connected() {
                warn!("BLE connection lost - returning to reconnect");
                return Err(ScaleError::NotConnected);
            }
        }
    }

    /// Clean up connection state
    async fn cleanup_connection(&mut self) {
        if let Some(connection) = &self.connection {
            if let Err(e) = self.ble_client.disconnect(connection).await {
                warn!("Failed to disconnect cleanly: {}", e);
            }
        }

        self.connection = None;
        self.weight_characteristic = None;
        self.command_characteristic = None;

        info!("Scale connection cleanup completed");
    }

    /// Check if currently connected to scale
    pub fn is_connected(&self) -> bool {
        self.connection.is_some()
    }

    /// Send tare command to scale
    pub async fn send_tare_command(&self) -> Result<(), ScaleError> {
        let command = [0x03, 0x0A, 0x01, 0x00, 0x00, 0x08]; // COMMAND_TARE from Python
        self.send_command(&command, "tare").await
    }

    /// Send start timer command to scale
    pub async fn send_start_timer_command(&self) -> Result<(), ScaleError> {
        let command = [0x03, 0x0A, 0x04, 0x00, 0x00, 0x0A]; // COMMAND_START_TIMER from Python
        self.send_command(&command, "start timer").await
    }

    /// Send stop timer command to scale
    pub async fn send_stop_timer_command(&self) -> Result<(), ScaleError> {
        let command = [0x03, 0x0A, 0x05, 0x00, 0x00, 0x0D]; // COMMAND_STOP_TIMER from Python
        self.send_command(&command, "stop timer").await
    }

    /// Send reset timer command to scale
    pub async fn send_reset_timer_command(&self) -> Result<(), ScaleError> {
        let command = [0x03, 0x0A, 0x06, 0x00, 0x00, 0x0C]; // COMMAND_RESET_TIMER from Python
        self.send_command(&command, "reset timer").await
    }

    /// Send a command to the scale via BLE
    async fn send_command(&self, command: &[u8; 6], command_name: &str) -> Result<(), ScaleError> {
        if !self.is_connected() {
            return Err(ScaleError::NotConnected);
        }

        let connection = self.connection.as_ref().unwrap();

        if let Some(ref command_char) = self.command_characteristic {
            info!("Sending {} command: {:02X?}", command_name, command);

            if let Err(e) = self
                .ble_client
                .write_characteristic(connection, command_char, command)
                .await
            {
                error!("Failed to send {} command: {:?}", command_name, e);
                return Err(ScaleError::from(e));
            }

            info!("{} command sent successfully", command_name);
            Ok(())
        } else {
            warn!(
                "Command characteristic not available - cannot send {} command",
                command_name
            );
            Err(ScaleError::CharacteristicNotFound)
        }
    }

    /// Monitor scale for incoming data and process commands
    async fn monitor_scale_data_with_commands(
        &self,
        command_channel: Arc<ScaleCommandChannel>,
    ) -> Result<(), ScaleError> {
        info!("Monitoring scale for weight data and commands...");

        let mut no_data_count = 0;
        const MAX_NO_DATA_COUNT: u32 = 300; // 5 minutes without data

        loop {
            // Check for commands with a timeout so we don't block data processing
            match embassy_futures::select::select(
                command_channel.receive(),
                Timer::after(Duration::from_millis(100)),
            )
            .await
            {
                embassy_futures::select::Either::First(command) => {
                    self.handle_command(command).await;
                }
                embassy_futures::select::Either::Second(_) => {
                    // Timer expired, continue to data processing
                }
            }

            // Check for new notification data
            if let Some(data) = self.ble_client.get_notification_data() {
                no_data_count = 0;

                debug!("Received scale data: {} bytes: {:02X?}", data.len(), data);

                // Parse the scale data
                if let Some(scale_data) = parse_scale_data(&data) {
                    info!(
                        "Parsed weight: {:.2}g, flow: {:.2}g/s, battery: {}%, timer: {}",
                        scale_data.weight_g,
                        scale_data.flow_rate_g_per_s,
                        scale_data.battery_percent,
                        scale_data.timer_running
                    );

                    // Send data to the main application
                    if let Err(_) = self.data_channel.try_send(scale_data) {
                        warn!("Failed to send scale data - channel full");
                    }
                } else {
                    warn!(
                        "Failed to parse scale data: {} bytes: {:02X?}",
                        data.len(),
                        data
                    );
                }
            } else {
                no_data_count += 1;

                // Log status every 30 seconds
                if no_data_count % 300 == 0 {
                    info!(
                        "Waiting for scale data... ({} seconds without data)",
                        no_data_count / 10
                    );
                }

                // Timeout after 5 minutes without data
                if no_data_count > MAX_NO_DATA_COUNT {
                    warn!("No data received from scale for 5 minutes - reconnecting");
                    return Err(ScaleError::BleError(BleError::ConnectionFailed(
                        "No data timeout".to_string(),
                    )));
                }
            }

            // Check if still connected
            if self.connection.is_none() {
                return Err(ScaleError::NotConnected);
            }

            // Check BLE connection status from the client
            if !self.ble_client.is_connected() {
                warn!("BLE connection lost - returning to reconnect");
                return Err(ScaleError::NotConnected);
            }
        }
    }

    /// Handle incoming scale commands
    async fn handle_command(&self, command: ScaleCommand) {
        match command {
            ScaleCommand::Tare => {
                info!("Processing tare command from channel");
                if let Err(e) = self.send_tare_command().await {
                    warn!("Failed to execute tare command: {:?}", e);
                }
            }
            ScaleCommand::StartTimer => {
                info!("Processing start timer command from channel");
                if let Err(e) = self.send_start_timer_command().await {
                    warn!("Failed to execute start timer command: {:?}", e);
                }
            }
            ScaleCommand::StopTimer => {
                info!("Processing stop timer command from channel");
                if let Err(e) = self.send_stop_timer_command().await {
                    warn!("Failed to execute stop timer command: {:?}", e);
                }
            }
            ScaleCommand::ResetTimer => {
                info!("Processing reset timer command from channel");
                if let Err(e) = self.send_reset_timer_command().await {
                    warn!("Failed to execute reset timer command: {:?}", e);
                }
            }
        }
    }
}

// Implement SmartScale trait
impl SmartScale for BookooScale {
    fn get_info(&self) -> &ScaleInfo {
        &self.info
    }

    fn is_connected(&self) -> bool {
        self.connection.is_some()
    }
}

// Implement BleScale trait
impl BleScale for BookooScale {
    fn get_ble_name_pattern(&self) -> &str {
        "BOOKOO_SC"
    }

    fn get_service_uuid(&self) -> uuid::Uuid {
        // Convert from 16-bit to full UUID
        uuid::Uuid::from_u128(0x0000_0FFE_0000_1000_8000_00805F9B34FB)
    }

    fn get_data_characteristic_uuid(&self) -> uuid::Uuid {
        // Convert from 16-bit to full UUID
        uuid::Uuid::from_u128(0x0000_FF11_0000_1000_8000_00805F9B34FB)
    }

    fn get_command_characteristic_uuid(&self) -> Option<uuid::Uuid> {
        // Convert from 16-bit to full UUID
        Some(uuid::Uuid::from_u128(
            0x0000_FF12_0000_1000_8000_00805F9B34FB,
        ))
    }

    fn parse_data(&self, raw_data: &[u8]) -> Result<ScaleData, Box<dyn std::error::Error>> {
        parse_scale_data(raw_data).ok_or_else(|| {
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Failed to parse scale data",
            )) as Box<dyn std::error::Error>
        })
    }

    fn format_command(&self, command: ScaleCommand) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let cmd_bytes = match command {
            ScaleCommand::Tare => [0x10, 0x00, 0x00, 0x00, 0x00, 0x10],
            ScaleCommand::StartTimer => [0x03, 0x00, 0x00, 0x00, 0x00, 0x03],
            ScaleCommand::StopTimer => [0x04, 0x00, 0x00, 0x00, 0x00, 0x04],
            ScaleCommand::ResetTimer => [0x05, 0x00, 0x00, 0x00, 0x00, 0x05],
        };
        Ok(cmd_bytes.to_vec())
    }
}
