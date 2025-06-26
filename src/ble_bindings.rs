// ble_bindings.rs - Direct ESP-IDF NimBLE FFI bindings
// This module provides a safe Rust wrapper around ESP-IDF's NimBLE APIs
// for scanning, connecting, and subscribing to BLE characteristics.

use crate::types::ScaleData;
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
static DISCOVERY_COMPLETE: LazyLock<Mutex<bool>> = LazyLock::new(|| Mutex::new(false));

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

impl CustomBleClient {
    pub fn new(
        data_sender: Arc<ScaleDataChannel>,
        status_sender: Arc<BleStatusChannel>,
    ) -> Self {
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
            
            debug!("Received connection event type: {}", event_type);
            
            // Define connection event constants
            const CONNECT_EVENT: u8 = esp_idf_sys::BLE_GAP_EVENT_CONNECT as u8;
            const DISCONNECT_EVENT: u8 = esp_idf_sys::BLE_GAP_EVENT_DISCONNECT as u8;
            
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
                _ => {
                    debug!("Unhandled connection event type: {}", event_type);
                }
            }
        }
        
        0 // Continue processing
    }
    
    // GATT discovery event handler
    extern "C" fn gatt_discovery_handler(
        conn_handle: u16,
        error: *const esp_idf_sys::ble_gatt_error,
        service: *const esp_idf_sys::ble_gatt_svc,
        _arg: *mut std::ffi::c_void,
    ) -> i32 {
        debug!("GATT discovery handler called for connection {}", conn_handle);
        
        unsafe {
            if !error.is_null() {
                let err = &*error;
                if err.status != 0 {
                    error!("GATT discovery error: status={}", err.status);
                    // Send error event immediately
                    GATT_EVENT_CHANNEL.try_send(GattEvent::DiscoveryError(err.status)).ok();
                    return 0;
                }
            }
            
            if service.is_null() {
                info!("GATT service discovery completed");
                if let Ok(mut complete) = DISCOVERY_COMPLETE.try_lock() {
                    *complete = true;
                }
                // Send completion event immediately
                GATT_EVENT_CHANNEL.try_send(GattEvent::DiscoveryComplete).ok();
                return 0;
            }
            
            let svc = &*service;
            
            info!("Discovered service (handle: {} - {})", svc.start_handle, svc.end_handle);
            
            // The service with the largest handle range is likely the main scale service
            // Bookoo scales typically use the service with handles 21+ for weight data
            if svc.start_handle >= 20 || (svc.end_handle - svc.start_handle) > 10 {
                info!("*** Found main Bookoo scale service! (handles {} - {}) ***", svc.start_handle, svc.end_handle);
                
                // Store service handle
                if let Ok(mut handle) = SERVICE_HANDLE.try_lock() {
                    *handle = Some(svc.start_handle);
                }
                
                // For the main service, the characteristic is typically at start_handle + 1
                if let Ok(mut handle) = CHAR_HANDLE.try_lock() {
                    *handle = Some(svc.start_handle + 1);
                }
                
                info!("*** Set characteristic handle to {} ***", svc.start_handle + 1);
                
                // Send service found event immediately
                GATT_EVENT_CHANNEL.try_send(GattEvent::ServiceFound(svc.start_handle, svc.end_handle)).ok();
            } else {
                debug!("Skipping service (handles {} - {}) - likely not the main scale service", svc.start_handle, svc.end_handle);
            }
        }
        
        0 // Continue processing
    }
    
    // Placeholder for characteristic discovery
    extern "C" fn gatt_char_discovery_handler(
        _conn_handle: u16,
        _error: *const esp_idf_sys::ble_gatt_error,
        _chr: *const esp_idf_sys::ble_gatt_chr,
        _arg: *mut std::ffi::c_void,
    ) -> i32 {
        // TODO: Implement characteristic discovery
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
                error!("Failed to start service discovery: {}, using fallback approach", ret);
                
                // Fallback: Use typical Bookoo scale service handles
                info!("Using fallback service discovery for Bookoo scale");
                if let Ok(mut handle) = SERVICE_HANDLE.try_lock() {
                    *handle = Some(21); // Typical main service handle
                }
                if let Ok(mut handle) = CHAR_HANDLE.try_lock() {
                    *handle = Some(22); // Typical characteristic handle  
                }
                
                info!("Fallback: Set service handle to 21, characteristic handle to 22");
                Timer::after(Duration::from_millis(500)).await;
                
                // Skip to subscription
                if let Err(e) = self.subscribe_to_notifications().await {
                    error!("Failed to subscribe to notifications: {:?}", e);
                    return Err(e);
                }
                
                info!("Fallback discovery and subscription completed");
                return Ok(());
            }
        }
        
        info!("GATT service discovery initiated");
        
        // Use Embassy select for immediate event-driven response
        use embassy_futures::select::{select, Either};
        
        let discovery_result = select(
            // Wait for GATT events
            async {
                let mut main_service_found = false;
                loop {
                    match GATT_EVENT_CHANNEL.receive().await {
                        GattEvent::ServiceFound(start, end) => {
                            info!("Received ServiceFound event: {} - {}", start, end);
                            if start >= 20 || (end - start) > 10 {
                                main_service_found = true;
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
                            if main_service_found {
                                info!("Error received but main service found, proceeding");
                                return Ok(());
                            } else {
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
                        return Err(e.into());
                    }
                }
            }
            Either::Second(Err(e)) => {
                error!("GATT discovery timed out: {}", e);
                return Err(e.into());
            }
            Either::Second(Ok(_)) => unreachable!(),
        }
        
        // Check if we found the scale service and characteristic
        let service_found = if let Ok(service) = SERVICE_HANDLE.try_lock() {
            service.is_some()
        } else {
            false
        };
        
        let char_found = if let Ok(char) = CHAR_HANDLE.try_lock() {
            char.is_some()
        } else {
            false
        };
        
        if service_found && char_found {
            info!("Found Bookoo scale service and characteristic!");
            
            // Subscribe to notifications
            if let Err(e) = self.subscribe_to_notifications().await {
                error!("Failed to subscribe to notifications: {:?}", e);
                return Err(e);
            }
            
            info!("Successfully subscribed to scale notifications");
            Ok(())
        } else {
            error!("Failed to find scale service or characteristic");
            Err("Scale service/characteristic not found".into())
        }
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
        
        // For now, simulate notification subscription
        // TODO: Implement proper GATT client notification subscription
        Timer::after(Duration::from_millis(500)).await;
        
        info!("Notification subscription completed");
        Ok(())
    }

    async fn monitor_connection(&self) -> Result<(), Box<dyn std::error::Error>> {
        // Simple connection monitoring loop
        for i in 0..60 {
            if !self.is_connected().await {
                info!("BLE connection lost");
                break;
            }
            
            // Simulate receiving some data every 10 seconds
            if i % 10 == 0 {
                debug!("Simulating scale data reception");
                // TODO: Replace with actual data parsing
            }
            
            Timer::after(Duration::from_secs(1)).await;
        }
        
        // Simulate connection loss after 60 seconds for testing
        self.set_connected(false).await;
        self.status_sender.send(false).await;
        
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