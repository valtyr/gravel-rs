use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, mutex::Mutex};
use embassy_time::{Duration, Instant, Timer};
use esp_idf_svc::hal::gpio::{Gpio19, Output, PinDriver};
use log::{error, info, warn};
use std::sync::Arc;

pub struct RelayController {
    gpio_pin: PinDriver<'static, Gpio19, Output>,
    current_state: Arc<Mutex<CriticalSectionRawMutex, bool>>,
    last_command_time: Arc<Mutex<CriticalSectionRawMutex, Option<Instant>>>,
}

impl RelayController {
    pub fn new(gpio19: Gpio19) -> Result<Self, RelayError> {
        let mut pin = PinDriver::output(gpio19)
            .map_err(|e| RelayError::GpioError(format!("Failed to configure GPIO19: {:?}", e)))?;

        // Ensure relay starts in OFF state (safety)
        pin.set_low().map_err(|e| {
            RelayError::GpioError(format!("Failed to set initial low state: {:?}", e))
        })?;

        info!("Relay controller initialized on GPIO19 (active high)");

        Ok(Self {
            gpio_pin: pin,
            current_state: Arc::new(Mutex::new(false)),
            last_command_time: Arc::new(Mutex::new(None)),
        })
    }

    pub async fn turn_on(&mut self) -> Result<(), RelayError> {
        let mut state = self.current_state.lock().await;
        if *state {
            return Ok(()); // Already on
        }

        self.gpio_pin
            .set_high()
            .map_err(|e| RelayError::GpioError(format!("Failed to set GPIO high: {:?}", e)))?;

        *state = true;
        *self.last_command_time.lock().await = Some(Instant::now());

        info!("Relay turned ON (GPIO19 HIGH)");
        Ok(())
    }

    pub async fn turn_off(&mut self) -> Result<(), RelayError> {
        let mut state = self.current_state.lock().await;
        if !*state {
            return Ok(()); // Already off
        }

        self.gpio_pin
            .set_low()
            .map_err(|e| RelayError::GpioError(format!("Failed to set GPIO low: {:?}", e)))?;

        *state = false;
        *self.last_command_time.lock().await = Some(Instant::now());

        info!("Relay turned OFF (GPIO19 LOW)");
        Ok(())
    }

    pub fn turn_off_immediately(&mut self) -> Result<(), RelayError> {
        // Emergency stop - bypass async and set GPIO directly
        match self.gpio_pin.set_low() {
            Ok(_) => {
                // Update state synchronously for safety
                // Note: In emergency situations, we prioritize immediate GPIO control
                // State tracking will be updated when the async runtime is available
                error!("EMERGENCY: Relay turned OFF immediately (GPIO19 LOW)");
                Ok(())
            }
            Err(e) => {
                error!(
                    "CRITICAL: Failed to turn off relay immediately: GPIO error: {:?}",
                    e
                );
                Err(RelayError::GpioError(format!(
                    "Emergency stop failed: {:?}",
                    e
                )))
            }
        }
    }

    pub async fn is_on(&self) -> bool {
        *self.current_state.lock().await
    }

    pub async fn get_last_command_time(&self) -> Option<Instant> {
        *self.last_command_time.lock().await
    }

    pub async fn test_relay(&mut self) -> Result<(), RelayError> {
        info!("Testing relay GPIO functionality");

        // Test sequence: OFF -> ON -> OFF
        self.gpio_pin
            .set_low()
            .map_err(|e| RelayError::GpioError(format!("Test: Failed to set low: {:?}", e)))?;

        Timer::after(Duration::from_millis(100)).await;

        self.gpio_pin
            .set_high()
            .map_err(|e| RelayError::GpioError(format!("Test: Failed to set high: {:?}", e)))?;

        Timer::after(Duration::from_millis(100)).await;

        self.gpio_pin
            .set_low()
            .map_err(|e| RelayError::GpioError(format!("Test: Failed to set low: {:?}", e)))?;

        // Reset state tracking
        *self.current_state.lock().await = false;

        info!("Relay GPIO test completed successfully");
        Ok(())
    }

    pub async fn force_state(&mut self, on: bool) -> Result<(), RelayError> {
        warn!("Force setting relay state to: {}", on);

        if on {
            self.gpio_pin
                .set_high()
                .map_err(|e| RelayError::GpioError(format!("Force ON failed: {:?}", e)))?;
        } else {
            self.gpio_pin
                .set_low()
                .map_err(|e| RelayError::GpioError(format!("Force OFF failed: {:?}", e)))?;
        }

        *self.current_state.lock().await = on;
        *self.last_command_time.lock().await = Some(Instant::now());

        Ok(())
    }
}

#[derive(Debug, Clone)]
pub enum RelayError {
    GpioError(String),
}

impl std::fmt::Display for RelayError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RelayError::GpioError(msg) => write!(f, "GPIO error: {}", msg),
        }
    }
}

impl std::error::Error for RelayError {}
