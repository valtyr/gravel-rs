use esp_idf_svc::hal::prelude::Peripherals;
use esp_idf_svc::bt::BtDriver;
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
    let nvs = EspDefaultNvsPartition::take().unwrap();
    
    // Initialize Bluetooth
    let bt_driver = match BtDriver::new(peripherals.modem, Some(nvs)) {
        Ok(driver) => driver,
        Err(e) => {
            log::error!("Failed to initialize BT driver: {:?}", e);
            return;
        }
    };
    
    // Create and start the controller
    let mut controller = match EspressoController::new(peripherals.pins.gpio19) {
        Ok(controller) => controller,
        Err(e) => {
            log::error!("Failed to create controller: {:?}", e);
            return;
        }
    };
    
    info!("Controller created successfully, starting...");
    
    // Start the controller with Embassy executor
    if let Err(e) = controller.start(spawner, bt_driver).await {
        log::error!("Controller start failed: {:?}", e);
    }
}
