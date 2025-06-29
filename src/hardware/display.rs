//! SH1106 OLED Display support for espresso scale controller
//! Using embedded-graphics for clean, efficient rendering

use embedded_graphics::{
    mono_font::{ascii::FONT_6X10, ascii::FONT_9X15, MonoTextStyle},
    pixelcolor::BinaryColor,
    prelude::*,
    primitives::{PrimitiveStyle, Rectangle},
    text::{Baseline, Text},
};
use esp_idf_svc::hal::{
    gpio::{InputPin, OutputPin},
    i2c::{I2cConfig, I2cDriver},
    peripheral::Peripheral,
    prelude::*,
};
use log::{debug, info};
use sh1106::Builder;

const DISPLAY_WIDTH: u32 = 128;
const DISPLAY_HEIGHT: u32 = 64;

// UI state for the display
#[derive(Debug, Clone)]
pub struct DisplayState {
    pub weight_g: f32,
    pub target_weight_g: f32,
    pub flow_rate_g_per_s: f32,
    pub timer_running: bool,
    pub brew_state: String,
    pub ble_connected: bool,
    pub battery_percent: u8,
    pub error: Option<String>,
}

impl Default for DisplayState {
    fn default() -> Self {
        Self {
            weight_g: 0.0,
            target_weight_g: 36.0,
            flow_rate_g_per_s: 0.0,
            timer_running: false,
            brew_state: "Idle".to_string(),
            ble_connected: false,
            battery_percent: 0,
            error: None,
        }
    }
}

pub struct DisplayController<I2C>
where
    I2C: embedded_hal::blocking::i2c::Write + embedded_hal::blocking::i2c::WriteRead,
{
    display: sh1106::mode::GraphicsMode<sh1106::interface::I2cInterface<I2C>>,
    state: DisplayState,
}

impl<I2C> DisplayController<I2C>
where
    I2C: embedded_hal::blocking::i2c::Write + embedded_hal::blocking::i2c::WriteRead,
    <I2C as embedded_hal::blocking::i2c::Write>::Error: std::fmt::Debug,
    <I2C as embedded_hal::blocking::i2c::WriteRead>::Error: std::fmt::Debug,
{
    pub fn new(i2c: I2C) -> Result<Self, Box<dyn std::error::Error>> {
        info!("Initializing SH1106 OLED display");

        let mut display: sh1106::mode::GraphicsMode<_> = Builder::new().connect_i2c(i2c).into();

        display
            .init()
            .map_err(|e| format!("Display init failed: {:?}", e))?;
        display.clear();
        display
            .flush()
            .map_err(|e| format!("Display flush failed: {:?}", e))?;

        info!("✅ SH1106 display initialized successfully");

        Ok(Self {
            display,
            state: DisplayState::default(),
        })
    }

    pub fn update_state(
        &mut self,
        new_state: DisplayState,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.state = new_state;
        self.refresh_display()
    }

    pub fn refresh_display(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        debug!("Refreshing display with current state");

        // Clear display
        self.display.clear();

        // Define text styles
        let title_style = MonoTextStyle::new(&FONT_9X15, BinaryColor::On);
        let text_style = MonoTextStyle::new(&FONT_6X10, BinaryColor::On);

        let mut y_pos = 15;

        // Error display takes priority
        if let Some(ref error) = self.state.error {
            Text::with_baseline("ERROR:", Point::new(0, y_pos), title_style, Baseline::Top)
                .draw(&mut self.display)
                .map_err(|e| format!("Display draw error: {:?}", e))?;
            y_pos += 16;

            Text::with_baseline(error, Point::new(0, y_pos), text_style, Baseline::Top)
                .draw(&mut self.display)
                .map_err(|e| format!("Display draw error: {:?}", e))?;
        } else {
            // Normal display layout

            // Line 1: Weight (large)
            let weight_text = format!("{:.1}g", self.state.weight_g);
            Text::with_baseline(
                &weight_text,
                Point::new(0, y_pos),
                title_style,
                Baseline::Top,
            )
            .draw(&mut self.display)
            .map_err(|e| format!("Display draw error: {:?}", e))?;

            // Target weight (smaller, right side)
            let target_text = format!("→{:.0}g", self.state.target_weight_g);
            Text::with_baseline(
                &target_text,
                Point::new(80, y_pos),
                text_style,
                Baseline::Top,
            )
            .draw(&mut self.display)
            .map_err(|e| format!("Display draw error: {:?}", e))?;
            y_pos += 18;

            // Line 2: Flow rate
            let flow_text = format!("Flow: {:.1}g/s", self.state.flow_rate_g_per_s);
            Text::with_baseline(&flow_text, Point::new(0, y_pos), text_style, Baseline::Top)
                .draw(&mut self.display)
                .map_err(|e| format!("Display draw error: {:?}", e))?;
            y_pos += 12;

            // Line 3: State and timer
            let state_text = format!(
                "{} {}",
                self.state.brew_state,
                if self.state.timer_running { "⏱" } else { "" }
            );
            Text::with_baseline(&state_text, Point::new(0, y_pos), text_style, Baseline::Top)
                .draw(&mut self.display)
                .map_err(|e| format!("Display draw error: {:?}", e))?;
            y_pos += 12;

            // Line 4: Status indicators
            let status_text = format!(
                "BLE:{} Bat:{}%",
                if self.state.ble_connected {
                    "✓"
                } else {
                    "✗"
                },
                self.state.battery_percent
            );
            Text::with_baseline(
                &status_text,
                Point::new(0, y_pos),
                text_style,
                Baseline::Top,
            )
            .draw(&mut self.display)
            .map_err(|e| format!("Display draw error: {:?}", e))?;
        }

        // Flush to display
        self.display
            .flush()
            .map_err(|e| format!("Display flush failed: {:?}", e))?;

        debug!("Display refresh completed");
        Ok(())
    }

    pub fn show_boot_screen(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        info!("Showing boot screen");

        self.display.clear();

        let title_style = MonoTextStyle::new(&FONT_9X15, BinaryColor::On);
        let text_style = MonoTextStyle::new(&FONT_6X10, BinaryColor::On);

        Text::with_baseline(
            "Gravel Scale",
            Point::new(10, 20),
            title_style,
            Baseline::Top,
        )
        .draw(&mut self.display)
        .map_err(|e| format!("Display draw error: {:?}", e))?;

        Text::with_baseline(
            "Initializing...",
            Point::new(20, 40),
            text_style,
            Baseline::Top,
        )
        .draw(&mut self.display)
        .map_err(|e| format!("Display draw error: {:?}", e))?;

        self.display
            .flush()
            .map_err(|e| format!("Display flush failed: {:?}", e))?;

        Ok(())
    }

    pub fn show_progress(
        &mut self,
        message: &str,
        progress: f32,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.display.clear();

        let text_style = MonoTextStyle::new(&FONT_6X10, BinaryColor::On);

        // Message
        Text::with_baseline(message, Point::new(0, 20), text_style, Baseline::Top)
            .draw(&mut self.display)
            .map_err(|e| format!("Display draw error: {:?}", e))?;

        // Progress bar
        let bar_width = ((DISPLAY_WIDTH - 20) as f32 * progress.clamp(0.0, 1.0)) as u32;

        // Progress bar outline
        Rectangle::new(Point::new(10, 35), Size::new(DISPLAY_WIDTH - 20, 8))
            .into_styled(PrimitiveStyle::with_stroke(BinaryColor::On, 1))
            .draw(&mut self.display)
            .map_err(|e| format!("Display draw error: {:?}", e))?;

        // Progress bar fill
        if bar_width > 2 {
            Rectangle::new(Point::new(11, 36), Size::new(bar_width - 2, 6))
                .into_styled(PrimitiveStyle::with_fill(BinaryColor::On))
                .draw(&mut self.display)
                .map_err(|e| format!("Display draw error: {:?}", e))?;
        }

        self.display
            .flush()
            .map_err(|e| format!("Display flush failed: {:?}", e))?;

        Ok(())
    }
}

// Helper function to create display controller from ESP32 I2C pins
pub fn create_display_controller(
    sda: impl Peripheral<P = impl InputPin + OutputPin> + 'static,
    scl: impl Peripheral<P = impl InputPin + OutputPin> + 'static,
) -> Result<DisplayController<I2cDriver<'static>>, Box<dyn std::error::Error>> {
    info!("Setting up I2C for SH1106 display");

    let config = I2cConfig::new().baudrate(400.kHz().into());
    let i2c = I2cDriver::new(
        unsafe { esp_idf_svc::hal::i2c::I2C0::new() },
        sda,
        scl,
        &config,
    )?;

    DisplayController::new(i2c)
}
