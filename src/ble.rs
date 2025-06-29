// ble.rs - Generic BLE client for ESP32-C6 using ESP-IDF NimBLE
// This module provides a reusable BLE client that can work with any BLE device

use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, channel::Channel};
use embassy_time::{Duration, Timer};
use log::{debug, error, info, warn};
use std::sync::{Arc, LazyLock, Mutex};

// ESP-IDF NimBLE bindings
use esp_idf_svc::sys as esp_idf_sys;

// BLE device address structure
#[derive(Debug, Clone)]
pub struct BleAddress {
    pub addr: [u8; 6],
    pub addr_type: u8,
}

// Discovered device information
#[derive(Debug, Clone)]
pub struct Device {
    pub name: Option<String>,
    pub address: BleAddress,
    pub rssi: i8,
}

// BLE service information
#[derive(Debug, Clone)]
pub struct Service {
    pub uuid: Uuid,
    pub start_handle: u16,
    pub end_handle: u16,
}

// BLE characteristic information
#[derive(Debug, Clone)]
pub struct Characteristic {
    pub uuid: Uuid,
    pub handle: u16,
    pub properties: u8,
}

// UUID type that supports both 16-bit and 128-bit UUIDs
#[derive(Debug, Clone, PartialEq)]
pub enum Uuid {
    Uuid16(u16),
    Uuid128([u8; 16]),
}

impl Uuid {
    pub fn from_u16(uuid: u16) -> Self {
        Uuid::Uuid16(uuid)
    }

    pub fn from_u128_bytes(bytes: [u8; 16]) -> Self {
        Uuid::Uuid128(bytes)
    }

    pub fn matches(&self, esp_uuid: &esp_idf_sys::ble_uuid_any_t) -> bool {
        unsafe {
            match self {
                Uuid::Uuid16(uuid) => {
                    if esp_uuid.u.type_ == esp_idf_sys::BLE_UUID_TYPE_16 as u8 {
                        esp_uuid.u16_.value == *uuid
                    } else {
                        false
                    }
                }
                Uuid::Uuid128(uuid_bytes) => {
                    if esp_uuid.u.type_ == esp_idf_sys::BLE_UUID_TYPE_128 as u8 {
                        let esp_uuid_bytes =
                            std::slice::from_raw_parts(esp_uuid.u128_.value.as_ptr(), 16);
                        esp_uuid_bytes == uuid_bytes
                    } else {
                        false
                    }
                }
            }
        }
    }
}

// BLE connection handle
#[derive(Debug, Clone)]
pub struct Connection {
    pub handle: u16,
}

// Device filter for scanning
pub struct DeviceFilter {
    pub name_prefix: Option<String>,
    pub service_uuid: Option<Uuid>,
}

// Channel types for notifications
pub type NotificationChannel<T> = Channel<CriticalSectionRawMutex, T, 10>;
pub type StatusChannel = Channel<CriticalSectionRawMutex, bool, 5>;

// Global scan state for C callbacks
static FOUND_DEVICES: LazyLock<Mutex<Vec<Device>>> = LazyLock::new(|| Mutex::new(Vec::new()));
static SCAN_COMPLETE: LazyLock<Mutex<bool>> = LazyLock::new(|| Mutex::new(false));

// Global connection state
static CONNECTION_HANDLE: LazyLock<Mutex<Option<u16>>> = LazyLock::new(|| Mutex::new(None));
static CONNECTED: LazyLock<Mutex<bool>> = LazyLock::new(|| Mutex::new(false));

// Global GATT discovery state
static DISCOVERED_SERVICES: LazyLock<Mutex<Vec<Service>>> =
    LazyLock::new(|| Mutex::new(Vec::new()));
static DISCOVERED_CHARACTERISTICS: LazyLock<Mutex<Vec<Characteristic>>> =
    LazyLock::new(|| Mutex::new(Vec::new()));
static DISCOVERY_COMPLETE: LazyLock<Mutex<bool>> = LazyLock::new(|| Mutex::new(false));

// Embassy channel for GATT events
type GattEventChannel = Channel<CriticalSectionRawMutex, GattEvent, 5>;
static GATT_EVENT_CHANNEL: LazyLock<GattEventChannel> = LazyLock::new(|| Channel::new());

#[derive(Clone, Debug)]
enum GattEvent {
    ServiceDiscovered(Service),
    CharacteristicDiscovered(Characteristic),
    DiscoveryComplete,
    DiscoveryError(u16),
}

// Global notification data storage
static NOTIFICATION_DATA: LazyLock<Mutex<Option<Vec<u8>>>> = LazyLock::new(|| Mutex::new(None));

// BLE error types
#[derive(Debug)]
pub enum BleError {
    InitializationFailed(String),
    ScanFailed(String),
    ConnectionFailed(String),
    DiscoveryFailed(String),
    SubscriptionFailed(String),
    NotConnected,
    DeviceNotFound,
}

impl std::fmt::Display for BleError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            BleError::InitializationFailed(msg) => write!(f, "BLE initialization failed: {}", msg),
            BleError::ScanFailed(msg) => write!(f, "BLE scan failed: {}", msg),
            BleError::ConnectionFailed(msg) => write!(f, "BLE connection failed: {}", msg),
            BleError::DiscoveryFailed(msg) => write!(f, "BLE discovery failed: {}", msg),
            BleError::SubscriptionFailed(msg) => write!(f, "BLE subscription failed: {}", msg),
            BleError::NotConnected => write!(f, "Not connected to device"),
            BleError::DeviceNotFound => write!(f, "Device not found"),
        }
    }
}

impl std::error::Error for BleError {}

// Generic BLE client implementation
pub struct BleClient {
    status_channel: Arc<StatusChannel>,
}

impl BleClient {
    pub fn new(status_channel: Arc<StatusChannel>) -> Self {
        Self { status_channel }
    }

    /// Initialize the BLE host stack (should be called once)
    pub fn initialize() -> Result<(), BleError> {
        info!("Initializing BLE host stack");

        unsafe {
            // Link ESP-IDF patches
            esp_idf_sys::link_patches();

            // Initialize NimBLE host
            let ret = esp_idf_sys::nimble_port_init();
            if ret != 0 {
                error!("nimble_port_init failed: {}", ret);
                return Err(BleError::InitializationFailed(format!(
                    "NimBLE init failed: {}",
                    ret
                )));
            }

            // Initialize host and controller
            esp_idf_sys::ble_hs_cfg.reset_cb = Some(Self::on_reset);
            esp_idf_sys::ble_hs_cfg.sync_cb = Some(Self::on_sync);
            esp_idf_sys::ble_hs_cfg.store_status_cb = Some(esp_idf_sys::ble_store_util_status_rr);

            // Start NimBLE host task
            esp_idf_sys::nimble_port_freertos_init(Some(Self::host_task));
        }

        info!("BLE host stack initialized successfully");
        Ok(())
    }

    /// Scan for BLE devices with optional filtering
    pub async fn scan_for_devices(
        &self,
        filter: Option<DeviceFilter>,
        duration_ms: u32,
    ) -> Result<Vec<Device>, BleError> {
        self.scan_for_devices_internal(filter, duration_ms, false)
            .await
    }

    /// Scan for the first matching device and return immediately
    pub async fn scan_for_first_device(
        &self,
        filter: Option<DeviceFilter>,
        duration_ms: u32,
    ) -> Result<Option<Device>, BleError> {
        let devices = self
            .scan_for_devices_internal(filter, duration_ms, true)
            .await?;
        Ok(devices.into_iter().next())
    }

    /// Internal scan implementation with early termination option
    async fn scan_for_devices_internal(
        &self,
        filter: Option<DeviceFilter>,
        duration_ms: u32,
        return_first: bool,
    ) -> Result<Vec<Device>, BleError> {
        info!("Starting BLE scan for {} ms", duration_ms);

        // Reset scan state
        FOUND_DEVICES.lock().unwrap().clear();
        *SCAN_COMPLETE.lock().unwrap() = false;

        unsafe {
            // Configure scan parameters
            let mut disc_params: esp_idf_sys::ble_gap_disc_params = std::mem::zeroed();
            disc_params.itvl = 96; // Scan interval (96 * 0.625ms = 60ms)
            disc_params.window = 48; // Scan window (48 * 0.625ms = 30ms)
            disc_params.filter_policy = 0; // No whitelist
            disc_params.set_passive(0); // Active scan
            disc_params.set_limited(0); // General discovery
            disc_params.set_filter_duplicates(1); // Filter duplicates

            // Get own address type
            let mut own_addr_type: u8 = 0;
            let ret = esp_idf_sys::ble_hs_id_infer_auto(0, &mut own_addr_type);
            if ret != 0 {
                return Err(BleError::ScanFailed(format!(
                    "Address type inference failed: {}",
                    ret
                )));
            }

            // Start discovery
            let ret = esp_idf_sys::ble_gap_disc(
                own_addr_type,
                duration_ms as i32,
                &disc_params,
                Some(Self::gap_event_handler),
                &filter as *const _ as *mut std::ffi::c_void,
            );

            if ret != 0 {
                return Err(BleError::ScanFailed(format!("Discovery failed: {}", ret)));
            }
        }

        // Wait for scan to complete or first device if requested
        let timeout_ms = duration_ms + 1000; // Add 1 second buffer
        let mut elapsed_ms = 0;

        loop {
            Timer::after(Duration::from_millis(100)).await;
            elapsed_ms += 100;

            let scan_complete = *SCAN_COMPLETE.lock().unwrap();
            let found_device = if return_first {
                !FOUND_DEVICES.lock().unwrap().is_empty()
            } else {
                false
            };

            if scan_complete || found_device || elapsed_ms > timeout_ms {
                if found_device && !scan_complete {
                    info!(
                        "Found target device early - stopping scan at {}ms",
                        elapsed_ms
                    );
                }
                break;
            }
        }

        // Stop scanning
        unsafe {
            esp_idf_sys::ble_gap_disc_cancel();
        }

        let devices = FOUND_DEVICES.lock().unwrap().clone();
        info!("Scan completed, found {} devices", devices.len());
        Ok(devices)
    }

    /// Connect to a specific device
    pub async fn connect(&self, device: &Device) -> Result<Connection, BleError> {
        info!("Connecting to device: {:?}", device.address);

        // Reset connection state
        *CONNECTION_HANDLE.lock().unwrap() = None;
        *CONNECTED.lock().unwrap() = false;

        unsafe {
            // Stop scanning first
            esp_idf_sys::ble_gap_disc_cancel();

            // Create BLE address structure
            let ble_addr = esp_idf_sys::ble_addr_t {
                type_: device.address.addr_type,
                val: device.address.addr,
            };

            // Set up connection parameters
            let conn_params = esp_idf_sys::ble_gap_conn_params {
                scan_itvl: 0x10,
                scan_window: 0x10,
                itvl_min: 24,
                itvl_max: 40,
                latency: 0,
                supervision_timeout: 256,
                min_ce_len: 0,
                max_ce_len: 0,
            };

            // Get own address type
            let mut own_addr_type: u8 = 0;
            let ret = esp_idf_sys::ble_hs_id_infer_auto(0, &mut own_addr_type);
            if ret != 0 {
                own_addr_type = esp_idf_sys::BLE_OWN_ADDR_PUBLIC as u8;
            }

            // Initiate connection
            let ret = esp_idf_sys::ble_gap_connect(
                own_addr_type,
                &ble_addr,
                30000, // 30 second timeout
                &conn_params,
                Some(Self::connection_event_handler),
                std::ptr::null_mut(),
            );

            if ret != 0 {
                return Err(BleError::ConnectionFailed(format!(
                    "Connection initiation failed: {}",
                    ret
                )));
            }
        }

        // Wait for connection to complete
        let mut timeout_counter = 0;
        loop {
            Timer::after(Duration::from_millis(50)).await;
            timeout_counter += 1;

            if *CONNECTED.lock().unwrap() {
                if let Some(handle) = *CONNECTION_HANDLE.lock().unwrap() {
                    info!("BLE connection established successfully");
                    self.status_channel.send(true).await;
                    return Ok(Connection { handle });
                }
            }

            if timeout_counter > 600 {
                // 30 second timeout
                return Err(BleError::ConnectionFailed("Connection timeout".into()));
            }
        }
    }

    /// Discover all services on a connection
    pub async fn discover_services(
        &self,
        connection: &Connection,
    ) -> Result<Vec<Service>, BleError> {
        info!("Discovering services on connection {}", connection.handle);

        // Reset discovery state
        DISCOVERED_SERVICES.lock().unwrap().clear();
        *DISCOVERY_COMPLETE.lock().unwrap() = false;

        unsafe {
            let ret = esp_idf_sys::ble_gattc_disc_all_svcs(
                connection.handle,
                Some(Self::gatt_discovery_handler),
                std::ptr::null_mut(),
            );

            if ret != 0 {
                return Err(BleError::DiscoveryFailed(format!(
                    "Service discovery failed: {}",
                    ret
                )));
            }
        }

        // Wait for discovery to complete with timeout
        use embassy_futures::select::{select, Either};

        let discovery_result = select(
            async {
                loop {
                    match GATT_EVENT_CHANNEL.receive().await {
                        GattEvent::ServiceDiscovered(_) => {
                            // Continue waiting for more services
                        }
                        GattEvent::DiscoveryComplete => {
                            return Ok::<(), String>(());
                        }
                        GattEvent::DiscoveryError(status) => {
                            if status == 14 {
                                warn!("Service discovery incomplete (status 14) - this is common");
                                return Ok(());
                            } else {
                                return Err(format!("Discovery error: {}", status));
                            }
                        }
                        _ => {}
                    }
                }
            },
            async {
                Timer::after(Duration::from_secs(10)).await;
                Err::<(), String>("Discovery timeout".to_string())
            },
        )
        .await;

        match discovery_result {
            Either::First(Ok(_)) => {
                let services = DISCOVERED_SERVICES.lock().unwrap().clone();
                info!("Discovered {} services", services.len());
                Ok(services)
            }
            Either::First(Err(e)) => Err(BleError::DiscoveryFailed(e)),
            Either::Second(Err(e)) => Err(BleError::DiscoveryFailed(e)),
            Either::Second(Ok(_)) => unreachable!(),
        }
    }

    /// Discover characteristics for a specific service
    pub async fn discover_characteristics(
        &self,
        connection: &Connection,
        service: &Service,
    ) -> Result<Vec<Characteristic>, BleError> {
        info!("Discovering characteristics for service {:?}", service.uuid);

        // Reset characteristic discovery state
        DISCOVERED_CHARACTERISTICS.lock().unwrap().clear();

        unsafe {
            let ret = esp_idf_sys::ble_gattc_disc_all_chrs(
                connection.handle,
                service.start_handle,
                service.end_handle,
                Some(Self::char_discovery_handler),
                std::ptr::null_mut(),
            );

            if ret != 0 {
                return Err(BleError::DiscoveryFailed(format!(
                    "Characteristic discovery failed: {}",
                    ret
                )));
            }
        }

        // Wait for characteristics to be discovered
        Timer::after(Duration::from_secs(3)).await;

        let characteristics = DISCOVERED_CHARACTERISTICS.lock().unwrap().clone();
        info!("Discovered {} characteristics", characteristics.len());
        Ok(characteristics)
    }

    /// Subscribe to notifications from a characteristic
    pub async fn subscribe_to_notifications(
        &self,
        connection: &Connection,
        characteristic: &Characteristic,
    ) -> Result<(), BleError> {
        info!(
            "Subscribing to notifications on characteristic handle {}",
            characteristic.handle
        );

        // Enable notifications via CCCD
        let cccd_handle = characteristic.handle + 1; // CCCD is typically at handle + 1
        let cccd_value: [u8; 2] = [0x01, 0x00]; // Enable notifications

        unsafe {
            let ret = esp_idf_sys::ble_gattc_write_flat(
                connection.handle,
                cccd_handle,
                cccd_value.as_ptr() as *const std::ffi::c_void,
                cccd_value.len() as u16,
                Some(Self::write_complete_handler),
                std::ptr::null_mut(),
            );

            if ret != 0 {
                return Err(BleError::SubscriptionFailed(format!(
                    "CCCD write failed: {}",
                    ret
                )));
            }
        }

        info!("Notification subscription initiated");
        Ok(())
    }

    /// Get the latest notification data (if any)
    pub fn get_notification_data(&self) -> Option<Vec<u8>> {
        NOTIFICATION_DATA.lock().unwrap().take()
    }

    /// Check if currently connected to a BLE device
    pub fn is_connected(&self) -> bool {
        *CONNECTED.lock().unwrap()
    }

    /// Write data to a characteristic
    pub async fn write_characteristic(
        &self,
        connection: &Connection,
        characteristic: &Characteristic,
        data: &[u8],
    ) -> Result<(), BleError> {
        info!(
            "Writing {} bytes to characteristic handle {}: {:02X?}",
            data.len(),
            characteristic.handle,
            data
        );

        unsafe {
            let ret = esp_idf_sys::ble_gattc_write_flat(
                connection.handle,
                characteristic.handle,
                data.as_ptr() as *const std::ffi::c_void,
                data.len() as u16,
                Some(Self::write_complete_handler),
                std::ptr::null_mut(),
            );

            if ret != 0 {
                return Err(BleError::SubscriptionFailed(format!(
                    "Characteristic write failed: {}",
                    ret
                )));
            }
        }

        info!("Characteristic write initiated");
        Ok(())
    }

    /// Disconnect from device
    pub async fn disconnect(&self, connection: &Connection) -> Result<(), BleError> {
        info!("Disconnecting from device");

        unsafe {
            let ret = esp_idf_sys::ble_gap_terminate(connection.handle, 0x13);
            if ret != 0 {
                warn!("Failed to initiate disconnection: {}", ret);
            }
        }

        // Reset state
        *CONNECTION_HANDLE.lock().unwrap() = None;
        *CONNECTED.lock().unwrap() = false;
        self.status_channel.send(false).await;

        info!("Disconnection completed");
        Ok(())
    }

    // BLE stack callbacks
    extern "C" fn on_reset(reason: i32) {
        error!("BLE host reset, reason: {}", reason);
    }

    extern "C" fn on_sync() {
        info!("BLE host synced");
    }

    extern "C" fn host_task(_param: *mut std::ffi::c_void) {
        unsafe {
            esp_idf_sys::nimble_port_run();
        }
    }

    // GAP event handler for scanning
    extern "C" fn gap_event_handler(
        event: *mut esp_idf_sys::ble_gap_event,
        arg: *mut std::ffi::c_void,
    ) -> i32 {
        if event.is_null() {
            return 0;
        }

        unsafe {
            let event_ref = &*event;
            match event_ref.type_ {
                x if x == esp_idf_sys::BLE_GAP_EVENT_DISC as u8 => {
                    let disc_data = &event_ref.__bindgen_anon_1.disc;

                    // Parse device name from advertisement data
                    let adv_data =
                        std::slice::from_raw_parts(disc_data.data, disc_data.length_data as usize);

                    if let Some(name) = Self::parse_device_name(adv_data) {
                        let device = Device {
                            name: Some(name.clone()),
                            address: BleAddress {
                                addr: disc_data.addr.val,
                                addr_type: disc_data.addr.type_,
                            },
                            rssi: disc_data.rssi,
                        };

                        // Apply filter if provided
                        let should_include = if !arg.is_null() {
                            let filter = &*(arg as *const Option<DeviceFilter>);
                            if let Some(ref filter) = filter {
                                if let Some(ref prefix) = filter.name_prefix {
                                    name.starts_with(prefix)
                                } else {
                                    true
                                }
                            } else {
                                true
                            }
                        } else {
                            true
                        };

                        if should_include {
                            info!("Found device: '{}' (RSSI: {})", name, disc_data.rssi);
                            FOUND_DEVICES.lock().unwrap().push(device);
                        }
                    }
                }
                x if x == esp_idf_sys::BLE_GAP_EVENT_DISC_COMPLETE as u8 => {
                    info!("BLE discovery completed");
                    *SCAN_COMPLETE.lock().unwrap() = true;
                }
                _ => {}
            }
        }

        0
    }

    // Connection event handler
    extern "C" fn connection_event_handler(
        event: *mut esp_idf_sys::ble_gap_event,
        _arg: *mut std::ffi::c_void,
    ) -> i32 {
        if event.is_null() {
            return 0;
        }

        unsafe {
            let event_ref = &*event;
            match event_ref.type_ as u32 {
                esp_idf_sys::BLE_GAP_EVENT_CONNECT => {
                    let conn_data = &event_ref.__bindgen_anon_1.connect;
                    if conn_data.status == 0 {
                        info!(
                            "BLE connection established! Handle: {}",
                            conn_data.conn_handle
                        );
                        *CONNECTION_HANDLE.lock().unwrap() = Some(conn_data.conn_handle);
                        *CONNECTED.lock().unwrap() = true;
                    } else {
                        error!("BLE connection failed with status: {}", conn_data.status);
                    }
                }
                esp_idf_sys::BLE_GAP_EVENT_DISCONNECT => {
                    let disconn_data = &event_ref.__bindgen_anon_1.disconnect;
                    info!(
                        "BLE disconnected! Handle: {}, Reason: {}",
                        disconn_data.conn.conn_handle, disconn_data.reason
                    );
                    *CONNECTION_HANDLE.lock().unwrap() = None;
                    *CONNECTED.lock().unwrap() = false;
                }
                esp_idf_sys::BLE_GAP_EVENT_NOTIFY_RX => {
                    let notify_data = &event_ref.__bindgen_anon_1.notify_rx;

                    if !notify_data.om.is_null() {
                        let om = &*notify_data.om;
                        let data_slice = std::slice::from_raw_parts(om.om_data, om.om_len as usize);

                        // Store notification data
                        *NOTIFICATION_DATA.lock().unwrap() = Some(data_slice.to_vec());
                        debug!("Received notification: {} bytes", data_slice.len());
                    }
                }
                _ => {}
            }
        }

        0
    }

    // GATT service discovery handler
    extern "C" fn gatt_discovery_handler(
        _conn_handle: u16,
        error: *const esp_idf_sys::ble_gatt_error,
        service: *const esp_idf_sys::ble_gatt_svc,
        _arg: *mut std::ffi::c_void,
    ) -> i32 {
        unsafe {
            if !error.is_null() {
                let err = &*error;
                if err.status != 0 {
                    GATT_EVENT_CHANNEL
                        .try_send(GattEvent::DiscoveryError(err.status))
                        .ok();
                    return 0;
                }
            }

            if service.is_null() {
                GATT_EVENT_CHANNEL
                    .try_send(GattEvent::DiscoveryComplete)
                    .ok();
                return 0;
            }

            let svc = &*service;
            let service_uuid = &svc.uuid;

            // Convert ESP UUID to our UUID type
            let uuid = if service_uuid.u.type_ == esp_idf_sys::BLE_UUID_TYPE_16 as u8 {
                Uuid::Uuid16(service_uuid.u16_.value)
            } else if service_uuid.u.type_ == esp_idf_sys::BLE_UUID_TYPE_128 as u8 {
                let uuid_bytes = std::slice::from_raw_parts(service_uuid.u128_.value.as_ptr(), 16);
                let mut bytes = [0u8; 16];
                bytes.copy_from_slice(uuid_bytes);
                Uuid::Uuid128(bytes)
            } else {
                return 0; // Unknown UUID type
            };

            let service = Service {
                uuid,
                start_handle: svc.start_handle,
                end_handle: svc.end_handle,
            };

            info!(
                "Discovered service: {:?} (handles {} - {})",
                service.uuid, service.start_handle, service.end_handle
            );

            DISCOVERED_SERVICES.lock().unwrap().push(service.clone());
            GATT_EVENT_CHANNEL
                .try_send(GattEvent::ServiceDiscovered(service))
                .ok();
        }

        0
    }

    // GATT characteristic discovery handler
    extern "C" fn char_discovery_handler(
        _conn_handle: u16,
        error: *const esp_idf_sys::ble_gatt_error,
        chr: *const esp_idf_sys::ble_gatt_chr,
        _arg: *mut std::ffi::c_void,
    ) -> i32 {
        unsafe {
            if !error.is_null() {
                let err = &*error;
                if err.status != 0 {
                    return 0;
                }
            }

            if chr.is_null() {
                return 0;
            }

            let chr_ref = &*chr;
            let char_uuid = &chr_ref.uuid;

            // Convert ESP UUID to our UUID type
            let uuid = if char_uuid.u.type_ == esp_idf_sys::BLE_UUID_TYPE_16 as u8 {
                Uuid::Uuid16(char_uuid.u16_.value)
            } else if char_uuid.u.type_ == esp_idf_sys::BLE_UUID_TYPE_128 as u8 {
                let uuid_bytes = std::slice::from_raw_parts(char_uuid.u128_.value.as_ptr(), 16);
                let mut bytes = [0u8; 16];
                bytes.copy_from_slice(uuid_bytes);
                Uuid::Uuid128(bytes)
            } else {
                return 0;
            };

            let characteristic = Characteristic {
                uuid,
                handle: chr_ref.val_handle,
                properties: chr_ref.properties,
            };

            info!(
                "Discovered characteristic: {:?} at handle {} (properties: 0x{:02X})",
                characteristic.uuid, characteristic.handle, characteristic.properties
            );

            DISCOVERED_CHARACTERISTICS
                .lock()
                .unwrap()
                .push(characteristic.clone());
            GATT_EVENT_CHANNEL
                .try_send(GattEvent::CharacteristicDiscovered(characteristic))
                .ok();
        }

        0
    }

    // Write completion handler
    extern "C" fn write_complete_handler(
        _conn_handle: u16,
        error: *const esp_idf_sys::ble_gatt_error,
        _attr: *mut esp_idf_sys::ble_gatt_attr,
        _arg: *mut std::ffi::c_void,
    ) -> i32 {
        unsafe {
            if !error.is_null() {
                let err = &*error;
                if err.status != 0 {
                    error!("GATT write failed: status={}", err.status);
                } else {
                    info!("GATT write completed successfully");
                }
            }
        }

        0
    }

    // Parse device name from advertisement data
    fn parse_device_name(adv_data: &[u8]) -> Option<String> {
        let mut offset = 0;

        while offset < adv_data.len() {
            if offset + 1 >= adv_data.len() {
                break;
            }

            let length = adv_data[offset] as usize;
            if length == 0 || offset + length >= adv_data.len() {
                break;
            }

            let ad_type = adv_data[offset + 1];

            // Check for complete local name (0x09) or shortened local name (0x08)
            if ad_type == 0x09 || ad_type == 0x08 {
                let name_data = &adv_data[offset + 2..offset + 1 + length];
                if let Ok(name) = std::str::from_utf8(name_data) {
                    return Some(name.to_string());
                }
            }

            offset += 1 + length;
        }

        None
    }
}
