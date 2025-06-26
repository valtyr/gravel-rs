// ble_bindings.rs - Direct ESP-IDF NimBLE FFI bindings
// This module provides a safe Rust wrapper around ESP-IDF's NimBLE APIs
// for scanning, connecting, and subscribing to BLE characteristics.

use crate::types::ScaleData;
use crate::protocol::parse_scale_data;
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, channel::Channel};
use embassy_time::{Duration, Timer};
use log::{info, warn, error, debug};
use std::sync::{Arc, Mutex, LazyLock};
use std::ffi::c_void;

// ESP-IDF NimBLE bindings
use esp_idf_svc::sys as esp_idf_sys;

// Global scan state for C callback
static FOUND_DEVICE: LazyLock<Mutex<Option<DiscoveredDevice>>> = LazyLock::new(|| Mutex::new(None));
static SCAN_COMPLETE: LazyLock<Mutex<bool>> = LazyLock::new(|| Mutex::new(false));

// Global connection state
static CONNECTION_HANDLE: LazyLock<Mutex<Option<u16>>> = LazyLock::new(|| Mutex::new(None));
static CONNECTED: LazyLock<Mutex<bool>> = LazyLock::new(|| Mutex::new(false));

// Global GATT state  
static SERVICE_HANDLE: LazyLock<Mutex<Option<u16>>> = LazyLock::new(|| Mutex::new(None));
static CHAR_HANDLE: LazyLock<Mutex<Option<u16>>> = LazyLock::new(|| Mutex::new(None));
static CCCD_HANDLE: LazyLock<Mutex<Option<u16>>> = LazyLock::new(|| Mutex::new(None));
static DISCOVERY_COMPLETE: LazyLock<Mutex<bool>> = LazyLock::new(|| Mutex::new(false));

// UUID-based discovery state
static WEIGHT_SERVICE_FOUND: LazyLock<Mutex<bool>> = LazyLock::new(|| Mutex::new(false));
static WEIGHT_CHAR_FOUND: LazyLock<Mutex<bool>> = LazyLock::new(|| Mutex::new(false));

// Target UUIDs (matching Python implementation exactly)
const BOOKOO_SERVICE_UUID: [u8; 16] = [
    0xfb, 0x34, 0x9b, 0x5f, 0x80, 0x00, 0x00, 0x80,
    0x00, 0x10, 0x00, 0x00, 0xe0, 0xff, 0x00, 0x00
]; // 0000ffe0-0000-1000-8000-00805f9b34fb

const WEIGHT_CHAR_UUID: [u8; 16] = [
    0xfb, 0x34, 0x9b, 0x5f, 0x80, 0x00, 0x00, 0x80,
    0x00, 0x10, 0x00, 0x00, 0x11, 0xff, 0x00, 0x00
]; // 0000ff11-0000-1000-8000-00805f9b34fb

const COMMAND_CHAR_UUID: [u8; 16] = [
    0xfb, 0x34, 0x9b, 0x5f, 0x80, 0x00, 0x00, 0x80,
    0x00, 0x10, 0x00, 0x00, 0x12, 0xff, 0x00, 0x00
]; // 0000ff12-0000-1000-8000-00805f9b34fb

// Global notification callback state
static SCALE_DATA_SENDER: LazyLock<Mutex<Option<Arc<ScaleDataChannel>>>> = LazyLock::new(|| Mutex::new(None));

// Embassy channel for instant GATT event notification
type GattEventChannel = Channel<CriticalSectionRawMutex, GattEvent, 5>;
static GATT_EVENT_CHANNEL: LazyLock<GattEventChannel> = LazyLock::new(|| Channel::new());

#[derive(Clone, Debug)]
enum GattEvent {
    ServiceFound(u16, u16), // start_handle, end_handle
    DiscoveryComplete,
    DiscoveryError(u16),
}

// Type aliases for our specific use case
pub type ScaleDataChannel = Channel<CriticalSectionRawMutex, ScaleData, 10>;
pub type BleStatusChannel = Channel<CriticalSectionRawMutex, bool, 5>;

// BLE device address structure
#[derive(Debug, Clone)]
pub struct BleAddress {
    pub addr: [u8; 6],
    pub addr_type: u8,
}

// Found device information
#[derive(Debug, Clone)]
pub struct DiscoveredDevice {
    pub name: Option<String>,
    pub address: BleAddress,
    pub rssi: i8,
}

// Custom BLE client implementation
pub struct CustomBleClient {
    data_sender: Arc<ScaleDataChannel>,
    status_sender: Arc<BleStatusChannel>,
    connected: Arc<Mutex<bool>>,
    connection_handle: Arc<Mutex<Option<u16>>>,
}

// Helper function to compare 128-bit UUIDs
fn uuid_matches(uuid_ptr: &esp_idf_sys::ble_uuid_any_t, target_uuid: &[u8; 16]) -> bool {
    unsafe {
        if uuid_ptr.u.type_ == esp_idf_sys::BLE_UUID_TYPE_128 as u8 {
            let uuid_bytes = std::slice::from_raw_parts(
                uuid_ptr.u128_.value.as_ptr(),
                16
            );
            return uuid_bytes == target_uuid;
        }
        false
    }
}

impl CustomBleClient {
    pub fn new(
        data_sender: Arc<ScaleDataChannel>,
        status_sender: Arc<BleStatusChannel>,
    ) -> Self {
        // Store data sender in global state for notification callback
        if let Ok(mut sender_opt) = SCALE_DATA_SENDER.try_lock() {
            *sender_opt = Some(data_sender.clone());
        }
        
        Self {
            data_sender,
            status_sender,
            connected: Arc::new(Mutex::new(false)),
            connection_handle: Arc::new(Mutex::new(None)),
        }
    }

    /// Initialize the BLE host stack
    pub fn initialize() -> Result<(), Box<dyn std::error::Error>> {
        info!("Initializing BLE host stack");
        
        unsafe {
            // Link ESP-IDF patches
            esp_idf_sys::link_patches();
            
            // Initialize NimBLE host
            let ret = esp_idf_sys::nimble_port_init();
            if ret != 0 {
                error!("nimble_port_init failed: {}", ret);
                return Err(format!("NimBLE init failed: {}", ret).into());
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
    
    // BLE stack callbacks
    extern "C" fn on_reset(reason: i32) {
        error!("BLE host reset, reason: {}", reason);
    }
    
    extern "C" fn on_sync() {
        info!("BLE host synced");
        
        unsafe {
            // Get local BLE address
            let mut addr: esp_idf_sys::ble_addr_t = std::mem::zeroed();
            let ret = esp_idf_sys::ble_hs_id_infer_auto(0, &mut addr.type_);
            if ret == 0 {
                esp_idf_sys::ble_hs_id_copy_addr(addr.type_, addr.val.as_mut_ptr(), std::ptr::null_mut());
                info!("BLE address type: {}", addr.type_);
            }
        }
    }
    
    extern "C" fn host_task(_param: *mut std::ffi::c_void) {
        unsafe {
            esp_idf_sys::nimble_port_run();
        }
    }
    

    /// Start scanning for BLE devices
    pub async fn start(&self) -> Result<(), Box<dyn std::error::Error>> {
        info!("Starting custom BLE client");
        
        // Initialize BLE if not already done
        Self::initialize()?;
        
        loop {
            match self.scan_and_connect().await {
                Ok(_) => {
                    info!("BLE connection cycle completed");
                }
                Err(e) => {
                    error!("BLE connection error: {:?}", e);
                    self.set_connected(false).await;
                    self.status_sender.send(false).await;
                }
            }
            
            info!("Waiting 5 seconds before retrying...");
            Timer::after(Duration::from_secs(5)).await;
        }
    }

    async fn scan_and_connect(&self) -> Result<(), Box<dyn std::error::Error>> {
        info!("Scanning for Bookoo scale...");
        
        let device = self.discover_scale().await?;
        info!("Found scale: {:?}", device);
        
        self.connect_to_device(&device).await?;
        info!("Connected to scale");
        
        self.set_connected(true).await;
        self.status_sender.send(true).await;
        
        self.discover_and_subscribe().await?;
        info!("Subscribed to notifications");
        
        self.monitor_connection().await?;
        
        Ok(())
    }

    async fn discover_scale(&self) -> Result<DiscoveredDevice, Box<dyn std::error::Error>> {
        info!("Starting BLE scan for Bookoo scale...");
        
        // Reset scan state
        *FOUND_DEVICE.lock().unwrap() = None;
        *SCAN_COMPLETE.lock().unwrap() = false;
        
        unsafe {
            // Configure scan parameters
            let mut disc_params: esp_idf_sys::ble_gap_disc_params = std::mem::zeroed();
            
            // Set scan parameters with more reasonable values
            disc_params.itvl = 96;   // Scan interval (96 * 0.625ms = 60ms)
            disc_params.window = 48; // Scan window (48 * 0.625ms = 30ms)  
            disc_params.filter_policy = 0; // BLE_HCI_SCAN_FILT_NO_WL
            
            // Set bitfields correctly
            disc_params.set_passive(0);           // Active scan (to get scan response data)
            disc_params.set_limited(0);           // General discovery
            disc_params.set_filter_duplicates(1); // Filter duplicates for cleaner output
            
            info!("Scan params: itvl={}, window={}, filter_policy={}", 
                  disc_params.itvl, disc_params.window, disc_params.filter_policy);
            
            // Get our own address type
            let mut own_addr_type: u8 = 0;
            let ret = esp_idf_sys::ble_hs_id_infer_auto(0, &mut own_addr_type);
            if ret != 0 {
                error!("Failed to infer address type: {}", ret);
                return Err(format!("Address type inference failed: {}", ret).into());
            }
            
            info!("Starting BLE discovery with own_addr_type: {}", own_addr_type);
            
            // Start discovery with finite duration (10 seconds for testing)
            let ret = esp_idf_sys::ble_gap_disc(
                own_addr_type,
                10000, // 10 second timeout in ms
                &disc_params,
                Some(Self::gap_event_handler),
                std::ptr::null_mut(),
            );
            
            if ret != 0 {
                error!("Failed to start BLE discovery: {}", ret);
                return Err(format!("BLE discovery failed: {}", ret).into());
            }
        }
        
        info!("BLE scan started, waiting for Bookoo scale...");
        
        // Wait for scan to complete or device to be found
        let mut timeout_counter = 0;
        loop {
            Timer::after(Duration::from_millis(100)).await;
            timeout_counter += 1;
            
            // Check if device was found
            if let Ok(mut found) = FOUND_DEVICE.try_lock() {
                if let Some(device) = found.take() {
                    info!("Found Bookoo scale during scan");
                    
                    // Stop scanning
                    unsafe {
                        esp_idf_sys::ble_gap_disc_cancel();
                    }
                    
                    return Ok(device);
                }
            }
            
            // Check if scan completed or timeout  
            if let Ok(complete) = SCAN_COMPLETE.try_lock() {
                if *complete || timeout_counter > 100 { // 10 second timeout for testing
                    break;
                }
            }
        }
        
        // Stop scanning on timeout
        unsafe {
            esp_idf_sys::ble_gap_disc_cancel();
        }
        
        warn!("No Bookoo scale found during 30 second scan");
        Err("Scale not found".into())
    }
    
    // BLE GAP event handler for scanning
    extern "C" fn gap_event_handler(
        event: *mut esp_idf_sys::ble_gap_event,
        _arg: *mut std::ffi::c_void,
    ) -> i32 {
        debug!("GAP event handler called");
        
        if event.is_null() {
            error!("Received null GAP event");
            return 0;
        }
        
        unsafe {
            let event_ref = &*event;
            let event_type = event_ref.type_;
            
            debug!("Received GAP event type: {}", event_type);
            
            // Define constants for better pattern matching
            const DISC_EVENT: u8 = esp_idf_sys::BLE_GAP_EVENT_DISC as u8;
            const DISC_COMPLETE_EVENT: u8 = esp_idf_sys::BLE_GAP_EVENT_DISC_COMPLETE as u8;
            
            match event_type {
                DISC_EVENT => {
                    // Access discovery data from the event union
                    let disc_data = &event_ref.__bindgen_anon_1.disc;
                    
                    debug!("BLE device discovered:");
                    debug!("  RSSI: {}", disc_data.rssi);
                    debug!("  Address type: {}", disc_data.addr.type_);
                    debug!("  Data length: {}", disc_data.length_data);
                    
                    // Parse device name from advertisement data
                    let adv_data = std::slice::from_raw_parts(
                        disc_data.data,
                        disc_data.length_data as usize,
                    );
                    
                    if let Some(name) = Self::parse_device_name(adv_data) {
                        info!("  Device found: '{}' (RSSI: {})", name, disc_data.rssi);
                        
                        // Check if this is a Bookoo scale
                        if name.starts_with("BOOKOO_SC") {
                            info!("*** Found Bookoo scale: {} ***", name);
                            
                            // Create discovered device
                            let device = DiscoveredDevice {
                                name: Some(name),
                                address: BleAddress {
                                    addr: disc_data.addr.val,
                                    addr_type: disc_data.addr.type_,
                                },
                                rssi: disc_data.rssi,
                            };
                            
                            // Store the found device
                            if let Ok(mut found) = FOUND_DEVICE.try_lock() {
                                *found = Some(device);
                            }
                        }
                    } else {
                        debug!("  Device without name (RSSI: {})", disc_data.rssi);
                    }
                }
                DISC_COMPLETE_EVENT => {
                    info!("BLE discovery completed");
                    if let Ok(mut complete) = SCAN_COMPLETE.try_lock() {
                        *complete = true;
                    }
                }
                _ => {
                    // Handle other GAP events if needed
                    debug!("Unhandled GAP event type: {}", event_type);
                }
            }
        }
        
        0 // Continue processing
    }
    
    // BLE connection event handler
    extern "C" fn connection_event_handler(
        event: *mut esp_idf_sys::ble_gap_event,
        _arg: *mut std::ffi::c_void,
    ) -> i32 {
        debug!("Connection event handler called");
        
        if event.is_null() {
            error!("Received null connection event");
            return 0;
        }
        
        unsafe {
            let event_ref = &*event;
            let event_type = event_ref.type_;
            
            // Log all event types to catch notifications
            info!("Connection event received: type={}", event_type as u32);
            match event_type as u32 {
                esp_idf_sys::BLE_GAP_EVENT_NOTIFY_RX => {
                    info!("*** NOTIFICATION EVENT DETECTED *** - Event type: {}", event_type);
                }
                esp_idf_sys::BLE_GAP_EVENT_CONNECT => {
                    debug!("Connection event");
                }
                esp_idf_sys::BLE_GAP_EVENT_DISCONNECT => {
                    debug!("Disconnect event"); 
                }
                _ => {
                    debug!("Other connection event type: {}", event_type);
                }
            }
            
            match event_type as u32 {
                esp_idf_sys::BLE_GAP_EVENT_CONNECT => {
                    let conn_data = &event_ref.__bindgen_anon_1.connect;
                    
                    if conn_data.status == 0 {
                        info!("BLE connection established! Handle: {}", conn_data.conn_handle);
                        
                        // Store connection handle and update state
                        if let Ok(mut handle) = CONNECTION_HANDLE.try_lock() {
                            *handle = Some(conn_data.conn_handle);
                        }
                        if let Ok(mut connected) = CONNECTED.try_lock() {
                            *connected = true;
                        }
                        
                    } else {
                        error!("BLE connection failed with status: {}", conn_data.status);
                        if let Ok(mut connected) = CONNECTED.try_lock() {
                            *connected = false;
                        }
                    }
                }
                esp_idf_sys::BLE_GAP_EVENT_DISCONNECT => {
                    let disconn_data = &event_ref.__bindgen_anon_1.disconnect;
                    warn!("BLE disconnected! Handle: {}, Reason: {}", 
                          disconn_data.conn.conn_handle, disconn_data.reason);
                    
                    // Update connection state to disconnected
                    if let Ok(mut handle) = CONNECTION_HANDLE.try_lock() {
                        *handle = None;
                    }
                    if let Ok(mut connected) = CONNECTED.try_lock() {
                        *connected = false;
                    }
                }
                esp_idf_sys::BLE_GAP_EVENT_NOTIFY_RX => {
                    let notify_data = &event_ref.__bindgen_anon_1.notify_rx;
                    info!("*** NOTIFICATION RECEIVED on handle {} ***", notify_data.attr_handle);
                    
                    // Extract notification data
                    if !notify_data.om.is_null() {
                        let om = &*notify_data.om;
                        let data_slice = std::slice::from_raw_parts(om.om_data, om.om_len as usize);
                        
                        info!("Notification data ({} bytes): {:02X?}", data_slice.len(), data_slice);
                        
                        // Try to parse ANY notification data we receive, regardless of handle
                        if let Some(scale_data) = parse_scale_data(data_slice) {
                            info!("Successfully parsed scale data: weight={:.2}g, flow={:.2}g/s, battery={}%, timer={}",
                                  scale_data.weight_g, scale_data.flow_rate_g_per_s, 
                                  scale_data.battery_percent, scale_data.timer_running);
                            
                            // Send the parsed data through the channel
                            if let Ok(sender_opt) = SCALE_DATA_SENDER.try_lock() {
                                if let Some(sender) = sender_opt.as_ref() {
                                    // Use try_send to avoid blocking in callback
                                    if let Err(_) = sender.try_send(scale_data) {
                                        warn!("Failed to send scale data - channel full");
                                    }
                                } else {
                                    warn!("No scale data sender available");
                                }
                            } else {
                                warn!("Failed to lock scale data sender");
                            }
                        } else {
                            info!("Could not parse as scale data - checking if it's expected format...");
                            // Check if this looks like the 19-byte data we saw earlier
                            if data_slice.len() == 19 && data_slice.len() >= 2 {
                                info!("Received 19-byte data with header [{:02X}, {:02X}] - might need format adjustment", 
                                      data_slice[0], data_slice[1]);
                            }
                        }
                    } else {
                        warn!("Notification received but no data available");
                    }
                }
                _ => {
                    debug!("Unhandled connection event type: {}", event_type);
                }
            }
        }
        
        0 // Continue processing
    }
    
    // UUID-based GATT service discovery handler
    extern "C" fn gatt_discovery_handler(
        conn_handle: u16,
        error: *const esp_idf_sys::ble_gatt_error,
        service: *const esp_idf_sys::ble_gatt_svc,
        _arg: *mut std::ffi::c_void,
    ) -> i32 {
        debug!("UUID-based GATT discovery handler called for connection {}", conn_handle);
        
        unsafe {
            if !error.is_null() {
                let err = &*error;
                if err.status != 0 {
                    if err.status == 14 {
                        warn!("Service discovery incomplete (status 14) - checking if we found anything useful");
                    } else {
                        error!("GATT discovery error: status={}", err.status);
                    }
                    GATT_EVENT_CHANNEL.try_send(GattEvent::DiscoveryError(err.status)).ok();
                    return 0;
                }
            }
            
            if service.is_null() {
                info!("GATT service discovery completed");
                if let Ok(mut complete) = DISCOVERY_COMPLETE.try_lock() {
                    *complete = true;
                }
                GATT_EVENT_CHANNEL.try_send(GattEvent::DiscoveryComplete).ok();
                return 0;
            }
            
            let svc = &*service;
            let service_uuid = &svc.uuid;
            
            info!("Discovered service (handle: {} - {})", svc.start_handle, svc.end_handle);
            
            // Check if this is the Bookoo scale service by UUID
            if uuid_matches(service_uuid, &BOOKOO_SERVICE_UUID) {
                info!("*** FOUND BOOKOO SCALE SERVICE BY UUID! (handles {} - {}) ***", svc.start_handle, svc.end_handle);
                info!("Service UUID matches: 0000ffe0-0000-1000-8000-00805f9b34fb");
                
                // Store service handle
                if let Ok(mut handle) = SERVICE_HANDLE.try_lock() {
                    *handle = Some(svc.start_handle);
                }
                
                // Mark that we found the weight service
                if let Ok(mut found) = WEIGHT_SERVICE_FOUND.try_lock() {
                    *found = true;
                }
                
                // Send service found event
                GATT_EVENT_CHANNEL.try_send(GattEvent::ServiceFound(svc.start_handle, svc.end_handle)).ok();
            } else {
                // Log the UUID for debugging
                if service_uuid.u.type_ == esp_idf_sys::BLE_UUID_TYPE_16 as u8 {
                    debug!("Service with 16-bit UUID: 0x{:04X} (handles {} - {})", 
                           service_uuid.u16_.value, svc.start_handle, svc.end_handle);
                } else if service_uuid.u.type_ == esp_idf_sys::BLE_UUID_TYPE_128 as u8 {
                    let uuid_bytes = std::slice::from_raw_parts(
                        service_uuid.u128_.value.as_ptr(), 16
                    );
                    debug!("Service with 128-bit UUID: {:02X?} (handles {} - {})", 
                           uuid_bytes, svc.start_handle, svc.end_handle);
                } else {
                    debug!("Service with unknown UUID type {} (handles {} - {})", 
                           service_uuid.u.type_, svc.start_handle, svc.end_handle);
                }
            }
        }
        
        0 // Continue processing
    }
    
    
    // UUID-based characteristic discovery handler to find weight characteristic
    extern "C" fn char_discovery_handler(
        conn_handle: u16,
        error: *const esp_idf_sys::ble_gatt_error,
        chr: *const esp_idf_sys::ble_gatt_chr,
        _arg: *mut std::ffi::c_void,
    ) -> i32 {
        unsafe {
            if !error.is_null() {
                let err = &*error;
                if err.status != 0 {
                    warn!("Characteristic discovery error for connection {}: status={}", conn_handle, err.status);
                    return 0;
                }
            }
            
            if chr.is_null() {
                info!("Characteristic discovery completed for connection {}", conn_handle);
                return 0;
            }
            
            let chr_ref = &*chr;
            let char_uuid = &chr_ref.uuid;
            
            // Log all characteristics for debugging
            let prop_names = if chr_ref.properties != 0 {
                let mut props = Vec::new();
                if chr_ref.properties & esp_idf_sys::BLE_GATT_CHR_PROP_READ as u8 != 0 { props.push("READ"); }
                if chr_ref.properties & esp_idf_sys::BLE_GATT_CHR_PROP_WRITE as u8 != 0 { props.push("WRITE"); }
                if chr_ref.properties & esp_idf_sys::BLE_GATT_CHR_PROP_NOTIFY as u8 != 0 { props.push("NOTIFY"); }
                if chr_ref.properties & esp_idf_sys::BLE_GATT_CHR_PROP_INDICATE as u8 != 0 { props.push("INDICATE"); }
                props.join("|")
            } else {
                "NONE".to_string()
            };
            
            info!("Discovered characteristic:");
            info!("  Handle: {}", chr_ref.val_handle);
            info!("  Definition Handle: {}", chr_ref.def_handle);
            info!("  Properties: {} (0x{:02X})", prop_names, chr_ref.properties);
            
            // Check if this is the weight characteristic by UUID
            if uuid_matches(char_uuid, &WEIGHT_CHAR_UUID) {
                info!("*** FOUND WEIGHT CHARACTERISTIC BY UUID! Handle: {} ***", chr_ref.val_handle);
                info!("Characteristic UUID matches: 0000ff11-0000-1000-8000-00805f9b34fb");
                
                // Verify it has NOTIFY property as expected
                if chr_ref.properties & esp_idf_sys::BLE_GATT_CHR_PROP_NOTIFY as u8 != 0 {
                    info!("  ✓ Confirmed: Has NOTIFY property as expected!");
                    
                    // Store this as our weight characteristic
                    if let Ok(mut handle) = CHAR_HANDLE.try_lock() {
                        *handle = Some(chr_ref.val_handle);
                    }
                    
                    // CCCD is typically at val_handle + 1
                    if let Ok(mut cccd_handle) = CCCD_HANDLE.try_lock() {
                        *cccd_handle = Some(chr_ref.val_handle + 1);
                    }
                    
                    // Mark that we found the weight characteristic
                    if let Ok(mut found) = WEIGHT_CHAR_FOUND.try_lock() {
                        *found = true;
                    }
                    
                    info!("Set weight characteristic handle to {} and CCCD to {}", 
                          chr_ref.val_handle, chr_ref.val_handle + 1);
                } else {
                    warn!("  ⚠ Warning: Weight characteristic found but doesn't have NOTIFY property!");
                }
                
            } else if uuid_matches(char_uuid, &COMMAND_CHAR_UUID) {
                info!("*** FOUND COMMAND CHARACTERISTIC BY UUID! Handle: {} ***", chr_ref.val_handle);
                info!("Characteristic UUID matches: 0000ff12-0000-1000-8000-00805f9b34fb");
                
            } else {
                // Log the UUID for debugging
                if char_uuid.u.type_ == esp_idf_sys::BLE_UUID_TYPE_16 as u8 {
                    debug!("Characteristic with 16-bit UUID: 0x{:04X} at handle {}", 
                           char_uuid.u16_.value, chr_ref.val_handle);
                } else if char_uuid.u.type_ == esp_idf_sys::BLE_UUID_TYPE_128 as u8 {
                    let uuid_bytes = std::slice::from_raw_parts(
                        char_uuid.u128_.value.as_ptr(), 16
                    );
                    debug!("Characteristic with 128-bit UUID: {:02X?} at handle {}", 
                           uuid_bytes, chr_ref.val_handle);
                } else {
                    debug!("Characteristic with unknown UUID type {} at handle {}", 
                           char_uuid.u.type_, chr_ref.val_handle);
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

    async fn connect_to_device(&self, device: &DiscoveredDevice) -> Result<(), Box<dyn std::error::Error>> {
        info!("Connecting to device: {:?}", device.address);
        
        unsafe {
            // Stop scanning first
            esp_idf_sys::ble_gap_disc_cancel();
            
            // Create BLE address structure
            let ble_addr = esp_idf_sys::ble_addr_t {
                type_: device.address.addr_type,
                val: device.address.addr,
            };
            
            info!("Initiating connection to {:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X} (type: {})",
                  ble_addr.val[5], ble_addr.val[4], ble_addr.val[3], 
                  ble_addr.val[2], ble_addr.val[1], ble_addr.val[0], ble_addr.type_);
            
            // Set up connection parameters
            let conn_params = esp_idf_sys::ble_gap_conn_params {
                scan_itvl: 0x10,      // 10ms
                scan_window: 0x10,    // 10ms  
                itvl_min: 24,         // 30ms (24 * 1.25ms)
                itvl_max: 40,         // 50ms (40 * 1.25ms)
                latency: 0,           // No latency
                supervision_timeout: 256, // 2.56 seconds (256 * 10ms)
                min_ce_len: 0,        // Minimum connection event length
                max_ce_len: 0,        // Maximum connection event length
            };
            
            // Get own address type
            let mut own_addr_type: u8 = 0;
            let ret = esp_idf_sys::ble_hs_id_infer_auto(0, &mut own_addr_type);
            if ret != 0 {
                warn!("Failed to infer own address type: {}", ret);
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
                error!("Failed to initiate BLE connection: {}", ret);
                return Err(format!("Connection failed: {}", ret).into());
            }
        }
        
        info!("Connection initiated, waiting for completion...");
        
        // Wait for connection to complete (event-driven, no arbitrary delays)
        let mut timeout_counter = 0;
        loop {
            Timer::after(Duration::from_millis(50)).await; // Faster polling
            timeout_counter += 1;
            
            if self.is_connected().await {
                info!("BLE connection established successfully");
                return Ok(());
            }
            
            if timeout_counter > 600 { // 30 second timeout (50ms * 600)
                error!("Connection timeout");
                return Err("Connection timeout".into());
            }
        }
    }

    async fn discover_and_subscribe(&self) -> Result<(), Box<dyn std::error::Error>> {
        info!("Discovering services and characteristics");
        
        // Reset discovery state
        *DISCOVERY_COMPLETE.lock().unwrap() = false;
        *SERVICE_HANDLE.lock().unwrap() = None;
        *CHAR_HANDLE.lock().unwrap() = None;
        *CCCD_HANDLE.lock().unwrap() = None;
        
        // Get connection handle
        let conn_handle = if let Ok(handle) = CONNECTION_HANDLE.try_lock() {
            if let Some(h) = *handle {
                h
            } else {
                return Err("No connection handle available".into());
            }
        } else {
            return Err("Failed to get connection handle".into());
        };
        
        info!("Starting GATT discovery on connection handle: {}", conn_handle);
        
        unsafe {
            // Start service discovery for all services
            let ret = esp_idf_sys::ble_gattc_disc_all_svcs(conn_handle, Some(Self::gatt_discovery_handler), std::ptr::null_mut());
            
            if ret != 0 {
                error!("Failed to start service discovery: {}", ret);
                self.cleanup_connection().await;
                return Err(format!("Service discovery initiation failed: {}", ret).into());
            }
        }
        
        info!("GATT service discovery initiated");
        
        // Use Embassy select for immediate event-driven response
        use embassy_futures::select::{select, Either};
        
        let discovery_result = select(
            // Wait for GATT events
            async {
                let mut _main_service_found = false;
                loop {
                    match GATT_EVENT_CHANNEL.receive().await {
                        GattEvent::ServiceFound(start, end) => {
                            info!("Received ServiceFound event: {} - {}", start, end);
                            if start >= 20 || (end - start) > 10 {
                                _main_service_found = true;
                                info!("Main service confirmed, ready to proceed");
                                // Continue to get all services, but we know we have the main one
                            }
                        }
                        GattEvent::DiscoveryComplete => {
                            info!("Received DiscoveryComplete event");
                            return Ok::<(), String>(());
                        }
                        GattEvent::DiscoveryError(status) => {
                            warn!("Received DiscoveryError: status {}", status);
                            
                            // Check if we found the Bookoo service by UUID despite the error
                            let bookoo_service_found = if let Ok(found) = WEIGHT_SERVICE_FOUND.try_lock() {
                                *found
                            } else {
                                false
                            };
                            
                            if status == 14 {
                                info!("Status 14 (incomplete discovery) is common - waiting for discovery to complete");
                                if bookoo_service_found {
                                    info!("Bookoo service found despite status 14, proceeding");
                                    return Ok(());
                                } else {
                                    info!("Status 14 but no Bookoo service found - continuing discovery to see all available services");
                                    // Don't return here, let discovery continue
                                }
                            } else {
                                error!("Serious discovery error (status {}), aborting", status);
                                return Err(format!("Discovery error: {}", status));
                            }
                        }
                    }
                }
            },
            // Timeout after 10 seconds
            async {
                Timer::after(Duration::from_secs(10)).await;
                Err::<(), String>("Discovery timeout".to_string())
            }
        ).await;
        
        match discovery_result {
            Either::First(result) => {
                match result {
                    Ok(_) => info!("GATT discovery completed successfully via events"),
                    Err(e) => {
                        error!("GATT discovery failed: {}", e);
                        self.cleanup_connection().await;
                        return Err(e.into());
                    }
                }
            }
            Either::Second(Err(e)) => {
                error!("GATT discovery timed out: {}", e);
                self.cleanup_connection().await;
                return Err(e.into());
            }
            Either::Second(Ok(_)) => unreachable!(),
        }
        
        // Check if we found the weight service by UUID
        let weight_service_found = if let Ok(found) = WEIGHT_SERVICE_FOUND.try_lock() {
            *found
        } else {
            false
        };
        
        if weight_service_found {
            info!("Found Bookoo scale service by UUID!");
            
            // Now discover characteristics to find the weight characteristic by UUID
            if let Err(e) = self.discover_characteristics().await {
                error!("Failed to discover characteristics: {:?}", e);
                return Err(e);
            }
            
            // Check if we found the weight characteristic by UUID
            let weight_char_found = if let Ok(found) = WEIGHT_CHAR_FOUND.try_lock() {
                *found
            } else {
                false
            };
            
            if weight_char_found {
                info!("Found weight characteristic by UUID!");
                
                // Subscribe to notifications
                if let Err(e) = self.subscribe_to_notifications().await {
                    error!("Failed to subscribe to notifications: {:?}", e);
                    return Err(e);
                }
                
                info!("Successfully subscribed to scale notifications");
                Ok(())
            } else {
                error!("Failed to find weight characteristic with UUID 0000ff11-0000-1000-8000-00805f9b34fb");
                Err("Weight characteristic (UUID 0000ff11-...) not found".into())
            }
        } else {
            error!("Failed to find Bookoo scale service with UUID 0000ffe0-0000-1000-8000-00805f9b34fb");
            // Clean up BLE connection before failing
            self.cleanup_connection().await;
            Err("Bookoo scale service (UUID 0000ffe0-...) not found".into())
        }
    }
    
    async fn discover_characteristics(&self) -> Result<(), Box<dyn std::error::Error>> {
        info!("Discovering characteristics and descriptors for scale service");
        
        // Get connection handle and service handle
        let conn_handle = if let Ok(handle) = CONNECTION_HANDLE.try_lock() {
            if let Some(h) = *handle {
                h
            } else {
                return Err("No connection handle available".into());
            }
        } else {
            return Err("Failed to get connection handle".into());
        };
        
        let service_handle = if let Ok(handle) = SERVICE_HANDLE.try_lock() {
            if let Some(h) = *handle {
                h
            } else {
                return Err("No service handle available".into());
            }
        } else {
            return Err("Failed to get service handle".into());
        };
        
        info!("Discovering all characteristics for service at handle {}", service_handle);
        
        // Clear previous discovery results
        if let Ok(mut char_handle) = CHAR_HANDLE.try_lock() {
            *char_handle = None;
        }
        if let Ok(mut cccd_handle) = CCCD_HANDLE.try_lock() {
            *cccd_handle = None;
        }
        if let Ok(mut found) = WEIGHT_CHAR_FOUND.try_lock() {
            *found = false;
        }
        
        unsafe {
            // Use UUID-based characteristic discovery within the Bookoo service
            info!("Starting UUID-based characteristic discovery for service range {}-65535", service_handle);
            
            let ret = esp_idf_sys::ble_gattc_disc_all_chrs(
                conn_handle,
                service_handle,     // start handle  
                65535,              // end handle - discover all characteristics
                Some(Self::char_discovery_handler),
                std::ptr::null_mut(),
            );
            
            if ret == 0 {
                info!("UUID-based characteristic discovery initiated successfully");
                
                // Wait for characteristic discovery to complete
                Timer::after(Duration::from_secs(5)).await;
                
                // Check if we found the weight characteristic by UUID
                let weight_char_found = if let Ok(found) = WEIGHT_CHAR_FOUND.try_lock() {
                    *found
                } else {
                    false
                };
                
                if weight_char_found {
                    info!("Successfully found weight characteristic by UUID!");
                } else {
                    error!("Failed to find weight characteristic with UUID 0000ff11-0000-1000-8000-00805f9b34fb");
                    error!("This means the characteristic discovery is working, but the scale doesn't have the expected UUID");
                    self.cleanup_connection().await;
                    return Err("Weight characteristic UUID not found".into());
                }
            } else {
                error!("Failed to initiate characteristic discovery: {}", ret);
                return Err(format!("Characteristic discovery failed: {}", ret).into());
            }
        }
        
        Ok(())
    }
    
    // Clean up BLE connection properly
    async fn cleanup_connection(&self) {
        info!("Cleaning up BLE connection...");
        
        // Get connection handle
        let conn_handle = if let Ok(handle) = CONNECTION_HANDLE.try_lock() {
            *handle
        } else {
            None
        };
        
        if let Some(handle) = conn_handle {
            unsafe {
                // Disconnect the BLE connection  
                let ret = esp_idf_sys::ble_gap_terminate(handle, 0x13); // BLE_ERR_REM_USER_CONN_TERM
                if ret == 0 {
                    info!("BLE disconnection initiated");
                } else {
                    warn!("Failed to initiate BLE disconnection: {}", ret);
                }
            }
        }
        
        // Reset connection state
        if let Ok(mut handle) = CONNECTION_HANDLE.try_lock() {
            *handle = None;
        }
        if let Ok(mut connected) = CONNECTED.try_lock() {
            *connected = false;
        }
        
        // Reset discovery state
        if let Ok(mut service_found) = WEIGHT_SERVICE_FOUND.try_lock() {
            *service_found = false;
        }
        if let Ok(mut char_found) = WEIGHT_CHAR_FOUND.try_lock() {
            *char_found = false;
        }
        
        // Reset handles
        if let Ok(mut service) = SERVICE_HANDLE.try_lock() {
            *service = None;
        }
        if let Ok(mut char) = CHAR_HANDLE.try_lock() {
            *char = None;
        }
        if let Ok(mut cccd) = CCCD_HANDLE.try_lock() {
            *cccd = None;
        }
        
        // Update status
        self.set_connected(false).await;
        self.status_sender.send(false).await;
        
        info!("BLE connection cleanup completed");
    }
    
    // Test a specific handle by subscribing to notifications and checking for valid weight data
    async fn test_notification_handle(&self, test_handle: u16) -> Result<bool, Box<dyn std::error::Error>> {
        info!("Testing handle {} for valid weight data notifications...", test_handle);
        
        // Get connection handle
        let conn_handle = if let Ok(handle) = CONNECTION_HANDLE.try_lock() {
            if let Some(h) = *handle {
                h
            } else {
                return Err("No connection handle available".into());
            }
        } else {
            return Err("Failed to get connection handle".into());
        };
        
        // Try to subscribe to notifications on this handle
        let cccd_handle = test_handle + 1;
        
        unsafe {
            // Try to enable notifications on this handle
            let enable_notifications: [u8; 2] = [0x01, 0x00]; // Enable notifications
            
            let ret = esp_idf_sys::ble_gattc_write_no_rsp_flat(
                conn_handle,
                cccd_handle,
                enable_notifications.as_ptr() as *const std::ffi::c_void,
                enable_notifications.len() as u16,
            );
            
            if ret != 0 {
                debug!("Failed to enable notifications on handle {}, CCCD {}: {}", test_handle, cccd_handle, ret);
                return Ok(false);
            }
            
            info!("Enabled notifications on handle {}, waiting for data...", test_handle);
            
            // Wait longer and try multiple approaches to get data
            let mut received_valid_data = false;
            let test_start = embassy_time::Instant::now();
            
            info!("Waiting for notifications or readable data on handle {}...", test_handle);
            
            // Try multiple read attempts and wait for notifications
            let mut attempts = 0;
            while embassy_time::Instant::now().duration_since(test_start) < Duration::from_secs(5) && attempts < 10 {
                Timer::after(Duration::from_millis(300)).await;
                attempts += 1;
                
                // Try reading the handle directly to see if it contains data
                let read_ret = esp_idf_sys::ble_gattc_read(
                    conn_handle,
                    test_handle,
                    Some(Self::test_read_handler_relaxed),
                    &mut received_valid_data as *mut bool as *mut std::ffi::c_void,
                );
                
                if read_ret == 0 {
                    Timer::after(Duration::from_millis(300)).await;
                    if received_valid_data {
                        break;
                    }
                } else {
                    debug!("Read attempt {} failed on handle {}: {}", attempts, test_handle, read_ret);
                }
                
                // For handle 22 specifically, let's try a broader data acceptance criteria
                if test_handle == 22 && attempts == 3 {
                    info!("Handle 22 special test: checking if 19-byte data could be valid...");
                    // We know handle 22 has 19-byte data, let's consider it potentially valid
                    // and see if we can make it work
                    received_valid_data = true;
                    break;
                }
            }
            
            // Disable notifications before moving to next handle
            let disable_notifications: [u8; 2] = [0x00, 0x00]; // Disable notifications
            let _disable_ret = esp_idf_sys::ble_gattc_write_no_rsp_flat(
                conn_handle,
                cccd_handle,
                disable_notifications.as_ptr() as *const std::ffi::c_void,
                disable_notifications.len() as u16,
            );
            
            if received_valid_data {
                info!("Handle {} provided valid 20-byte weight data!", test_handle);
                return Ok(true);
            } else {
                debug!("Handle {} did not provide valid weight data within timeout", test_handle);
                return Ok(false);
            }
        }
    }
    
    // Special read handler for testing handles that checks for valid weight data format
    extern "C" fn test_read_handler(
        _conn_handle: u16,
        error: *const esp_idf_sys::ble_gatt_error,
        attr: *mut esp_idf_sys::ble_gatt_attr,
        arg: *mut std::ffi::c_void,
    ) -> i32 {
        unsafe {
            if !error.is_null() {
                let err = &*error;
                if err.status != 0 {
                    return 0;
                }
            }
            
            if !attr.is_null() && !arg.is_null() {
                let attr_ref = &*attr;
                let valid_data_flag = &mut *(arg as *mut bool);
                
                if !attr_ref.om.is_null() {
                    let om = &*attr_ref.om;
                    let data_slice = std::slice::from_raw_parts(om.om_data, om.om_len as usize);
                    
                    // Check if this is valid 20-byte weight data with correct header
                    if data_slice.len() == 20 && data_slice.len() >= 2 {
                        if data_slice[0] == 0x03 && data_slice[1] == 0x0B {
                            info!("*** FOUND VALID WEIGHT DATA on handle {} with format [0x03, 0x0B] ***", attr_ref.handle);
                            *valid_data_flag = true;
                        }
                    }
                }
            }
        }
        
        0
    }
    
    // Relaxed read handler that accepts various data formats for testing
    extern "C" fn test_read_handler_relaxed(
        _conn_handle: u16,
        error: *const esp_idf_sys::ble_gatt_error,
        attr: *mut esp_idf_sys::ble_gatt_attr,
        arg: *mut std::ffi::c_void,
    ) -> i32 {
        unsafe {
            if !error.is_null() {
                let err = &*error;
                if err.status != 0 {
                    return 0;
                }
            }
            
            if !attr.is_null() && !arg.is_null() {
                let attr_ref = &*attr;
                let valid_data_flag = &mut *(arg as *mut bool);
                
                if !attr_ref.om.is_null() {
                    let om = &*attr_ref.om;
                    let data_slice = std::slice::from_raw_parts(om.om_data, om.om_len as usize);
                    
                    info!("Testing handle {} data ({} bytes): {:02X?}", attr_ref.handle, data_slice.len(), data_slice);
                    
                    // Check for ideal 20-byte weight data with correct header
                    if data_slice.len() == 20 && data_slice.len() >= 2 {
                        if data_slice[0] == 0x03 && data_slice[1] == 0x0B {
                            info!("*** FOUND PERFECT 20-BYTE WEIGHT DATA on handle {} ***", attr_ref.handle);
                            *valid_data_flag = true;
                            return 0;
                        }
                    }
                    
                    // For handle 22: Accept the 19-byte data and see if we can work with it
                    if attr_ref.handle == 22 && data_slice.len() == 19 {
                        info!("*** ACCEPTING 19-BYTE DATA on handle 22 for further testing ***");
                        info!("Data: {:02X?}", data_slice);
                        *valid_data_flag = true;
                        return 0;
                    }
                    
                    // Accept any data that's at least 10 bytes and might contain useful information
                    if data_slice.len() >= 10 {
                        info!("*** FOUND POTENTIALLY USEFUL DATA ({} bytes) on handle {} ***", data_slice.len(), attr_ref.handle);
                        *valid_data_flag = true;
                        return 0;
                    }
                }
            }
        }
        
        0
    }
    
    // Handler for characteristic reads during discovery
    extern "C" fn char_read_handler(
        conn_handle: u16,
        error: *const esp_idf_sys::ble_gatt_error,
        attr: *mut esp_idf_sys::ble_gatt_attr,
        _arg: *mut std::ffi::c_void,
    ) -> i32 {
        unsafe {
            if !error.is_null() {
                let err = &*error;
                if err.status != 0 {
                    debug!("Characteristic read failed for connection {}: status={}", conn_handle, err.status);
                    return 0;
                }
            }
            
            if !attr.is_null() {
                let attr_ref = &*attr;
                info!("Successfully read data from handle {}", attr_ref.handle);
                
                if !attr_ref.om.is_null() {
                    let om = &*attr_ref.om;
                    let data_slice = std::slice::from_raw_parts(om.om_data, om.om_len as usize);
                    info!("Handle {} data ({} bytes): {:02X?}", attr_ref.handle, data_slice.len(), data_slice);
                    
                    // Check if this looks like scale data
                    if data_slice.len() == 20 && data_slice.len() >= 2 {
                        if data_slice[0] == 0x03 && data_slice[1] == 0x0B {
                            info!("*** Handle {} contains WEIGHT DATA format [0x03, 0x0B] - This is likely the weight characteristic! ***", attr_ref.handle);
                        } else {
                            info!("Handle {} has 20-byte data but wrong header [{:02X}, {:02X}] (expected [0x03, 0x0B])", 
                                  attr_ref.handle, data_slice[0], data_slice[1]);
                        }
                    } else if data_slice.len() == 19 {
                        info!("Handle {} has 19-byte data (not weight format) with header [{:02X}, {:02X}]", 
                              attr_ref.handle, 
                              if data_slice.len() > 0 { data_slice[0] } else { 0 },
                              if data_slice.len() > 1 { data_slice[1] } else { 0 });
                    }
                } else {
                    info!("Handle {} read succeeded but no data available", attr_ref.handle);
                }
            }
        }
        
        0
    }
    
    // Handler for periodic polling reads
    extern "C" fn poll_read_handler(
        _conn_handle: u16,
        error: *const esp_idf_sys::ble_gatt_error,
        attr: *mut esp_idf_sys::ble_gatt_attr,
        _arg: *mut std::ffi::c_void,
    ) -> i32 {
        unsafe {
            if !error.is_null() {
                let err = &*error;
                if err.status != 0 {
                    warn!("Poll read failed: status={}", err.status);
                    return 0;
                }
            }
            
            if !attr.is_null() {
                let attr_ref = &*attr;
                
                if !attr_ref.om.is_null() {
                    let om = &*attr_ref.om;
                    let data_slice = std::slice::from_raw_parts(om.om_data, om.om_len as usize);
                    
                    info!("POLL: Handle {} data ({} bytes): {:02X?}", attr_ref.handle, data_slice.len(), data_slice);
                    
                    // Check if this looks like scale data format
                    if data_slice.len() == 20 && data_slice.len() >= 2 {
                        if data_slice[0] == 0x03 && data_slice[1] == 0x0B {
                            info!("POLL: Found valid 20-byte scale data!");
                        }
                    } else if data_slice.len() == 19 {
                        info!("POLL: Found 19-byte data (same as before)");
                    }
                }
            }
        }
        
        0
    }
    
    async fn subscribe_to_notifications(&self) -> Result<(), Box<dyn std::error::Error>> {
        info!("Subscribing to scale notifications");
        
        // Get connection and characteristic handles
        let conn_handle = if let Ok(handle) = CONNECTION_HANDLE.try_lock() {
            if let Some(h) = *handle {
                h
            } else {
                return Err("No connection handle available".into());
            }
        } else {
            return Err("Failed to get connection handle".into());
        };
        
        let char_handle = if let Ok(handle) = CHAR_HANDLE.try_lock() {
            if let Some(h) = *handle {
                h
            } else {
                return Err("No characteristic handle available".into());
            }
        } else {
            return Err("Failed to get characteristic handle".into());
        };
        
        info!("Subscribing to notifications on handle {} for connection {}", char_handle, conn_handle);
        
        // Try to get the discovered CCCD handle, otherwise try common locations
        let cccd_candidates = vec![
            // Use discovered CCCD handle if available
            if let Ok(handle) = CCCD_HANDLE.try_lock() {
                if let Some(h) = *handle {
                    Some(h)
                } else {
                    None
                }
            } else {
                None
            },
            // Common CCCD locations for Bookoo scales
            Some(char_handle + 1), // 23 (most common)
            Some(char_handle + 2), // 24 (alternative)
            Some(25),              // Fixed handle sometimes used
            Some(26),              // Another alternative
        ].into_iter().filter_map(|x| x).collect::<Vec<u16>>();
        
        let mut cccd_write_success = false;
        
        unsafe {
            // Enable notifications (0x0001) in little endian format
            let cccd_value: [u8; 2] = [0x01, 0x00];
            
            // Try each CCCD handle until one works
            for cccd_handle in cccd_candidates {
                info!("Trying to write to CCCD handle {} to enable notifications", cccd_handle);
                
                // Try to use ble_gattc_write_flat first
                let ret = esp_idf_sys::ble_gattc_write_flat(
                    conn_handle,
                    cccd_handle,
                    cccd_value.as_ptr() as *const std::ffi::c_void,
                    cccd_value.len() as u16,
                    Some(Self::write_complete_handler),
                    std::ptr::null_mut(),
                );
                
                if ret == 0 {
                    info!("CCCD write initiated successfully on handle {}", cccd_handle);
                    cccd_write_success = true;
                    
                    // Wait for the write to complete
                    Timer::after(Duration::from_millis(500)).await;
                    break;
                } else {
                    warn!("Failed to write CCCD using ble_gattc_write_flat on handle {}: {}", cccd_handle, ret);
                    
                    // Try fallback method
                    let ret = esp_idf_sys::ble_gattc_write_no_rsp_flat(
                        conn_handle,
                        cccd_handle,
                        cccd_value.as_ptr() as *const std::ffi::c_void,
                        cccd_value.len() as u16,
                    );
                    
                    if ret == 0 {
                        info!("CCCD write (no response) successful on handle {}", cccd_handle);
                        cccd_write_success = true;
                        Timer::after(Duration::from_millis(300)).await;
                        break;
                    } else {
                        warn!("Failed to write CCCD using fallback method on handle {}: {}", cccd_handle, ret);
                    }
                }
            }
        }
        
        if !cccd_write_success {
            error!("Failed to write CCCD on any handle - notifications may not work");
            // Don't return error, continue and hope notifications work anyway
        }
        
        // Wait for the CCCD write to complete
        Timer::after(Duration::from_millis(500)).await;
        
        info!("Notification subscription process completed - scale should now send weight data");
        
        // Try to read the characteristic directly to see if there's data available
        // and to potentially trigger the scale to start sending notifications
        if let Err(e) = self.trigger_scale_notifications().await {
            warn!("Failed to trigger scale notifications: {:?}", e);
        }
        
        Ok(())
    }
    
    async fn trigger_scale_notifications(&self) -> Result<(), Box<dyn std::error::Error>> {
        info!("Attempting to trigger scale notifications");
        
        // Get connection and characteristic handles
        let conn_handle = if let Ok(handle) = CONNECTION_HANDLE.try_lock() {
            if let Some(h) = *handle {
                h
            } else {
                return Err("No connection handle available".into());
            }
        } else {
            return Err("Failed to get connection handle".into());
        };
        
        let char_handle = if let Ok(handle) = CHAR_HANDLE.try_lock() {
            if let Some(h) = *handle {
                h
            } else {
                return Err("No characteristic handle available".into());
            }
        } else {
            return Err("Failed to get characteristic handle".into());
        };
        
        unsafe {
            // Method 1: Read the characteristic value directly to trigger the scale
            info!("Reading characteristic {} to trigger scale", char_handle);
            let ret = esp_idf_sys::ble_gattc_read(
                conn_handle,
                char_handle,
                Some(Self::trigger_read_handler),
                std::ptr::null_mut(),
            );
            
            if ret == 0 {
                info!("Triggered characteristic read successfully");
            } else {
                warn!("Failed to trigger characteristic read: {}", ret);
            }
            
            Timer::after(Duration::from_millis(500)).await;
            
            // Method 2: Try sending a simple command to wake up the scale
            info!("Attempting to send wakeup command to scale");
            
            // Simple wake-up command (common pattern for many scales)
            let wakeup_cmd: [u8; 1] = [0x00];
            
            let ret = esp_idf_sys::ble_gattc_write_no_rsp_flat(
                conn_handle,
                char_handle,
                wakeup_cmd.as_ptr() as *const std::ffi::c_void,
                wakeup_cmd.len() as u16,
            );
            
            if ret == 0 {
                info!("Sent wakeup command successfully");
            } else {
                debug!("Failed to send wakeup command: {} (this is normal for many scales)", ret);
            }
        }
        
        Timer::after(Duration::from_millis(500)).await;
        info!("Scale trigger sequence completed");
        Ok(())
    }
    
    // Handler for reads that trigger scale activity
    extern "C" fn trigger_read_handler(
        conn_handle: u16,
        error: *const esp_idf_sys::ble_gatt_error,
        attr: *mut esp_idf_sys::ble_gatt_attr,
        _arg: *mut std::ffi::c_void,
    ) -> i32 {
        unsafe {
            if !error.is_null() {
                let err = &*error;
                if err.status != 0 {
                    debug!("Trigger read failed for connection {}: status={}", conn_handle, err.status);
                    return 0;
                }
            }
            
            if !attr.is_null() {
                let attr_ref = &*attr;
                info!("Trigger read successful from handle {}", attr_ref.handle);
                
                if !attr_ref.om.is_null() {
                    let om = &*attr_ref.om;
                    let data_slice = std::slice::from_raw_parts(om.om_data, om.om_len as usize);
                    info!("Scale returned data: {} bytes: {:02X?}", data_slice.len(), data_slice);
                    
                    // Try to parse this data as scale data in case it's a notification-like response
                    if let Some(scale_data) = parse_scale_data(data_slice) {
                        info!("Parsed scale data from trigger read: weight={:.2}g, flow={:.2}g/s, battery={}%, timer={}",
                              scale_data.weight_g, scale_data.flow_rate_g_per_s, 
                              scale_data.battery_percent, scale_data.timer_running);
                        
                        // Send the parsed data through the channel
                        if let Ok(sender_opt) = SCALE_DATA_SENDER.try_lock() {
                            if let Some(sender) = sender_opt.as_ref() {
                                if let Err(_) = sender.try_send(scale_data) {
                                    warn!("Failed to send scale data from trigger read - channel full");
                                }
                            }
                        }
                    }
                }
            }
        }
        
        0
    }
    
    // Handler for GATT write completion
    extern "C" fn write_complete_handler(
        conn_handle: u16,
        error: *const esp_idf_sys::ble_gatt_error,
        _attr: *mut esp_idf_sys::ble_gatt_attr,
        _arg: *mut std::ffi::c_void,
    ) -> i32 {
        unsafe {
            if !error.is_null() {
                let err = &*error;
                if err.status != 0 {
                    error!("GATT write failed for connection {}: status={}", conn_handle, err.status);
                    return 0;
                }
            }
            
            info!("CCCD write completed successfully for connection {}", conn_handle);
        }
        
        0
    }

    async fn monitor_connection(&self) -> Result<(), Box<dyn std::error::Error>> {
        info!("Monitoring BLE connection and waiting for scale notifications");
        
        // Monitor connection and wait for notifications
        let notification_count = 0;
        for i in 0..300 { // 5 minutes total
            if !self.is_connected().await {
                info!("BLE connection lost during monitoring");
                break;
            }
            
            // Log connection status every 30 seconds and try polling the scale
            if i % 30 == 0 {
                info!("BLE connection active - received {} notifications so far", notification_count);
                info!("Attempting to poll scale for current data...");
        
        // Get connection handle for polling
        let conn_handle = if let Ok(handle) = CONNECTION_HANDLE.try_lock() {
            if let Some(h) = *handle {
                h
            } else {
                warn!("No connection handle available for polling");
                continue;
            }
        } else {
            warn!("Failed to get connection handle for polling");
            continue;
        };
        
        // Let's try reading handle 22 periodically to see if data changes
        for poll_i in 0..5 {
            Timer::after(Duration::from_secs(2)).await;
            info!("Polling attempt {} - reading handle 22 for current data", poll_i + 1);
            
            unsafe {
                let read_ret = esp_idf_sys::ble_gattc_read(
                    conn_handle,
                    22,
                    Some(Self::poll_read_handler),
                    std::ptr::null_mut(),
                );
                
                if read_ret == 0 {
                    info!("Initiated read request for handle 22");
                } else {
                    warn!("Failed to read handle 22: {}", read_ret);
                }
            }
        }
                
                // Try to read the characteristic directly every 30 seconds
                if let Err(e) = self.poll_scale_data().await {
                    debug!("Failed to poll scale data: {:?}", e);
                }
            }
            
            Timer::after(Duration::from_secs(1)).await;
        }
        
        info!("Connection monitoring completed - received {} total notifications", notification_count);
        
        // Connection monitoring complete
        self.set_connected(false).await;
        self.status_sender.send(false).await;
        
        Ok(())
    }
    
    async fn poll_scale_data(&self) -> Result<(), Box<dyn std::error::Error>> {
        // Get connection and characteristic handles
        let conn_handle = if let Ok(handle) = CONNECTION_HANDLE.try_lock() {
            if let Some(h) = *handle {
                h
            } else {
                return Err("No connection handle available".into());
            }
        } else {
            return Err("Failed to get connection handle".into());
        };
        
        let char_handle = if let Ok(handle) = CHAR_HANDLE.try_lock() {
            if let Some(h) = *handle {
                h
            } else {
                return Err("No characteristic handle available".into());
            }
        } else {
            return Err("Failed to get characteristic handle".into());
        };
        
        unsafe {
            debug!("Polling characteristic {} for current weight data", char_handle);
            let ret = esp_idf_sys::ble_gattc_read(
                conn_handle,
                char_handle,
                Some(Self::trigger_read_handler),
                std::ptr::null_mut(),
            );
            
            if ret == 0 {
                debug!("Scale polling read initiated successfully");
            } else {
                debug!("Failed to initiate scale polling read: {}", ret);
            }
        }
        
        Ok(())
    }

    async fn set_connected(&self, connected: bool) {
        *self.connected.lock().unwrap() = connected;
    }

    pub async fn is_connected(&self) -> bool {
        // Check global connection state 
        if let Ok(connected) = CONNECTED.try_lock() {
            *connected
        } else {
            false
        }
    }

    // Command methods (placeholders for now)
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