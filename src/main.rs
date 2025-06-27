use esp_idf_svc::hal::prelude::Peripherals;
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use gravel_rs::controller::EspressoController;
use gravel_rs::wifi_manager::WifiManager;
use log::info;
use embassy_executor::Spawner;

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    // It is necessary to call this function once. Otherwise some patches to the runtime
    // implemented by esp-idf-sys might not link properly. See https://github.com/esp-rs/esp-idf-template/issues/71
    esp_idf_svc::sys::link_patches();

    // Bind the log crate to the ESP Logging facilities
    esp_idf_svc::log::EspLogger::initialize_default();

    info!("Starting Espresso Scale Controller");

    // Initialize peripherals
    let peripherals = Peripherals::take().unwrap();
    
    // Initialize networking stack with WiFi provisioning
    let nvs = EspDefaultNvsPartition::take().unwrap();
    let sys_loop = EspSystemEventLoop::take().unwrap();
    
    info!("Initializing WiFi Manager with BLE provisioning...");
    let mut wifi_manager = match WifiManager::new(peripherals.modem, sys_loop, nvs).await {
        Ok(manager) => {
            info!("WiFi Manager initialized successfully");
            Some(manager)
        }
        Err(e) => {
            log::warn!("WiFi Manager initialization failed: {:?} - continuing without WiFi", e);
            None
        }
    };
    
    // Start WiFi (provisioning or connection)
    let (wifi_connected, ble_needs_reset) = if let Some(ref mut manager) = wifi_manager {
        match manager.start().await {
            Ok((connected, needs_reset)) => {
                info!("📶 WiFi initialization completed - connected: {}, BLE reset needed: {}", connected, needs_reset);
                (connected, needs_reset)
            }
            Err(e) => {
                log::warn!("WiFi start failed: {:?} - continuing without WiFi", e);
                (false, false)
            }
        }
    } else {
        (false, false)
    };
    
    // Create and start the controller
    let mut controller = match EspressoController::new(peripherals.pins.gpio19).await {
        Ok(controller) => controller,
        Err(e) => {
            log::error!("Failed to create controller: {:?}", e);
            return;
        }
    };
    
    info!("Controller created successfully, starting...");
    
    // Start the controller with Embassy executor
    // Pass WiFi status and BLE reset flag
    if let Err(e) = controller.start(spawner, wifi_connected, ble_needs_reset).await {
        log::error!("Controller start failed: {:?}", e);
    }
}
