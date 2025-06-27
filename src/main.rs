use esp_idf_svc::hal::prelude::Peripherals;
use esp_idf_svc::wifi::{EspWifi, BlockingWifi};
use esp_idf_svc::wifi::Configuration;
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use gravel_rs::controller::EspressoController;
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
    
    // Initialize networking stack (required for HTTP server)
    let nvs = EspDefaultNvsPartition::take().unwrap();
    let sys_loop = EspSystemEventLoop::take().unwrap();
    
    info!("Initializing Wi-Fi for networking stack...");
    let _wifi = match initialize_wifi(peripherals.modem, nvs, sys_loop) {
        Ok(wifi) => {
            info!("Wi-Fi stack initialized successfully");
            Some(wifi)
        }
        Err(e) => {
            log::warn!("Wi-Fi initialization failed: {:?} - HTTP server may not work", e);
            None
        }
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
    if let Err(e) = controller.start(spawner).await {
        log::error!("Controller start failed: {:?}", e);
    }
}

fn initialize_wifi(
    modem: esp_idf_svc::hal::modem::Modem,
    nvs: EspDefaultNvsPartition,
    sys_loop: EspSystemEventLoop,
) -> Result<BlockingWifi<EspWifi<'static>>, Box<dyn std::error::Error>> {
    // Create Wi-Fi driver (this initializes the networking stack)
    let wifi = EspWifi::new(modem, sys_loop.clone(), Some(nvs))?;
    let mut wifi = BlockingWifi::wrap(wifi, sys_loop)?;
    
    // Set to station mode (client mode - not creating an access point)
    wifi.set_configuration(&Configuration::Client(Default::default()))?;
    
    // Start Wi-Fi (this doesn't connect to any network, just initializes the stack)
    wifi.start()?;
    
    info!("Wi-Fi networking stack initialized (not connected to any network)");
    Ok(wifi)
}
