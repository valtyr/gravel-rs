//! NVS (Non-Volatile Storage) persistence for brew settings and learning data.
//! Uses dedicated custom partition for app settings separate from WiFi.

use log::{info, warn, error, debug};
use serde::{Deserialize, Serialize};
use embassy_time::Instant;
use std::sync::Arc;
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, mutex::Mutex};
use esp_idf_svc::nvs::{EspNvs, EspNvsPartition, NvsCustom};
use esp_idf_svc::sys::EspError;

// Version for settings migration
const SETTINGS_VERSION: u8 = 1;

// NVS namespace for our application
const NVS_NAMESPACE: &str = "gravel_brew";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrewSettings {
    pub version: u8,
    pub target_weight_g: f32,
    pub auto_tare: bool,
    pub predictive_stop: bool,
    
    // Overshoot learning data
    pub overshoot_delay_ms: i32,
    pub overshoot_ewma: f32,         // Exponentially weighted moving average
    pub learning_confidence: f32,    // 0.0 to 1.0 confidence score
    
    // Timestamps
    pub last_updated: u64,           // Unix timestamp
    pub created_at: u64,             // When settings were first created
}

impl Default for BrewSettings {
    fn default() -> Self {
        let now = embassy_time::Instant::now().as_millis();
        Self {
            version: SETTINGS_VERSION,
            target_weight_g: 36.0,
            auto_tare: true,
            predictive_stop: true,
            overshoot_delay_ms: 500,     // Start with 500ms like Python
            overshoot_ewma: 0.0,         // No learned bias initially
            learning_confidence: 0.0,    // No confidence initially
            last_updated: now,
            created_at: now,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrewStatistics {
    pub version: u8,
    pub total_brews: u32,
    pub successful_predictions: u32,
    pub total_predictions: u32,
    pub average_overshoot_g: f32,
    pub best_delay_ms: i32,
    pub worst_overshoot_g: f32,
    pub total_brewing_time_ms: u64,
    pub last_brew_timestamp: u64,
}

impl Default for BrewStatistics {
    fn default() -> Self {
        Self {
            version: SETTINGS_VERSION,
            total_brews: 0,
            successful_predictions: 0,
            total_predictions: 0,
            average_overshoot_g: 0.0,
            best_delay_ms: 500,
            worst_overshoot_g: 0.0,
            total_brewing_time_ms: 0,
            last_brew_timestamp: 0,
        }
    }
}

pub struct NvsStorage {
    nvs: Option<Arc<Mutex<CriticalSectionRawMutex, EspNvs<NvsCustom>>>>,
    cached_settings: Arc<Mutex<CriticalSectionRawMutex, BrewSettings>>,
    cached_stats: Arc<Mutex<CriticalSectionRawMutex, BrewStatistics>>,
    mock_mode: bool,
}

impl NvsStorage {
    pub async fn new() -> Result<Self, Box<dyn std::error::Error>> {
        info!("🗄️ Initializing NVS storage for brew settings");
        
        // Try to initialize real NVS with custom partition
        let (nvs, mock_mode) = match Self::init_nvs() {
            Ok(nvs) => {
                info!("✅ Real NVS storage initialized successfully");
                (Some(Arc::new(Mutex::new(nvs))), false)
            }
            Err(e) => {
                warn!("⚠️ NVS initialization failed: {:?} - using in-memory storage", e);
                (None, true)
            }
        };
        
        let mut storage = Self {
            nvs,
            cached_settings: Arc::new(Mutex::new(BrewSettings::default())),
            cached_stats: Arc::new(Mutex::new(BrewStatistics::default())),
            mock_mode,
        };

        // Load existing data if NVS is available
        if !mock_mode {
            if let Err(e) = storage.load_from_nvs().await {
                warn!("Failed to load from NVS: {:?} - using defaults", e);
            }
        }

        info!("✅ NVS storage initialized (mock_mode: {})", mock_mode);
        Ok(storage)
    }
    
    fn init_nvs() -> Result<EspNvs<NvsCustom>, EspError> {
        // Try to use a custom NVS partition (separate from WiFi)
        // If custom partition doesn't exist, fall back to default
        let partition = EspNvsPartition::<NvsCustom>::take("nvs_custom")
            .or_else(|_| {
                info!("Custom NVS partition not found, using default NVS");
                EspNvsPartition::<NvsCustom>::take("nvs")
            })?;
        let nvs = EspNvs::new(partition, NVS_NAMESPACE, true)?;
        Ok(nvs)
    }
    
    async fn load_from_nvs(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(ref nvs_arc) = self.nvs {
            let nvs = nvs_arc.lock().await;
            
            // Load settings
            let mut buffer = vec![0u8; 1024]; // Buffer for reading
            if let Ok(Some(data)) = nvs.get_blob("settings", &mut buffer) {
                if let Ok(settings) = serde_json::from_slice::<BrewSettings>(data) {
                    *self.cached_settings.lock().await = settings;
                    info!("📂 Loaded brew settings from NVS");
                }
            }
            
            // Load statistics  
            let mut buffer = vec![0u8; 1024]; // Buffer for reading
            if let Ok(Some(data)) = nvs.get_blob("statistics", &mut buffer) {
                if let Ok(stats) = serde_json::from_slice::<BrewStatistics>(data) {
                    *self.cached_stats.lock().await = stats;
                    info!("📊 Loaded brew statistics from NVS");
                }
            }
        }
        Ok(())
    }

    /// Get current settings (from cache)
    pub async fn get_settings(&self) -> BrewSettings {
        self.cached_settings.lock().await.clone()
    }

    /// Get current statistics (from cache)
    pub async fn get_statistics(&self) -> BrewStatistics {
        self.cached_stats.lock().await.clone()
    }

    /// Update settings in cache and persist to NVS
    pub async fn update_settings(&self, settings: BrewSettings) -> Result<(), Box<dyn std::error::Error>> {
        // Update cache
        {
            let mut cached = self.cached_settings.lock().await;
            *cached = settings.clone();
        }

        // Persist to NVS if available
        if let Some(ref nvs_arc) = self.nvs {
            let mut nvs = nvs_arc.lock().await;
            let data = serde_json::to_vec(&settings)?;
            nvs.set_blob("settings", &data)?;
            debug!("💾 Saved settings to NVS: target={:.1}g, delay={}ms, ewma={:.2}g", 
                   settings.target_weight_g, settings.overshoot_delay_ms, settings.overshoot_ewma);
        } else {
            debug!("📝 [MOCK] Would save settings to NVS: target={:.1}g, delay={}ms, ewma={:.2}g", 
                   settings.target_weight_g, settings.overshoot_delay_ms, settings.overshoot_ewma);
        }
        
        Ok(())
    }

    /// Update specific overshoot learning parameters
    pub async fn update_overshoot_learning(
        &self, 
        delay_ms: i32, 
        ewma: f32, 
        confidence: f32
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut settings = self.get_settings().await;
        settings.overshoot_delay_ms = delay_ms;
        settings.overshoot_ewma = ewma;
        settings.learning_confidence = confidence;
        settings.last_updated = Instant::now().as_millis();

        debug!("🧠 [MOCK] Updating overshoot learning: delay={}ms, ewma={:.2}g, confidence={:.2}", 
               delay_ms, ewma, confidence);

        self.update_settings(settings).await
    }

    /// Update brewing statistics in cache and persist to NVS
    pub async fn update_statistics(&self, stats: BrewStatistics) -> Result<(), Box<dyn std::error::Error>> {
        // Update cache
        {
            let mut cached = self.cached_stats.lock().await;
            *cached = stats.clone();
        }

        // Persist to NVS if available
        if let Some(ref nvs_arc) = self.nvs {
            let mut nvs = nvs_arc.lock().await;
            let data = serde_json::to_vec(&stats)?;
            nvs.set_blob("statistics", &data)?;
            debug!("💾 Saved statistics to NVS: {} brews, {}/{} predictions successful", 
                   stats.total_brews, stats.successful_predictions, stats.total_predictions);
        } else {
            debug!("📊 [MOCK] Would save statistics to NVS: {} brews, {}/{} predictions successful", 
                   stats.total_brews, stats.successful_predictions, stats.total_predictions);
        }
        
        Ok(())
    }

    /// Record a completed brew with overshoot data
    pub async fn record_brew(&self, overshoot_g: f32, prediction_used: bool, prediction_successful: bool) {
        let mut stats = self.get_statistics().await;
        
        stats.total_brews += 1;
        stats.last_brew_timestamp = Instant::now().as_millis();
        
        if prediction_used {
            stats.total_predictions += 1;
            if prediction_successful {
                stats.successful_predictions += 1;
            }
        }

        // Update overshoot statistics
        let old_avg = stats.average_overshoot_g;
        stats.average_overshoot_g = (old_avg * (stats.total_brews - 1) as f32 + overshoot_g) / stats.total_brews as f32;
        
        if overshoot_g.abs() > stats.worst_overshoot_g.abs() {
            stats.worst_overshoot_g = overshoot_g;
        }

        info!("📊 Brew #{} recorded: overshoot={:.1}g, prediction={}, success={}, avg_overshoot={:.1}g",
              stats.total_brews, overshoot_g, prediction_used, prediction_successful, stats.average_overshoot_g);

        if let Err(e) = self.update_statistics(stats).await {
            warn!("Failed to save brew statistics: {:?}", e);
        }
    }

    /// Get a summary of learning progress for logging
    pub async fn get_learning_summary(&self) -> String {
        let settings = self.get_settings().await;
        let stats = self.get_statistics().await;
        
        let success_rate = if stats.total_predictions > 0 {
            (stats.successful_predictions as f32 / stats.total_predictions as f32) * 100.0
        } else {
            0.0
        };

        format!(
            "Learning Summary: {} brews, delay={}ms, ewma={:.1}g, confidence={:.1}%, success_rate={:.1}%",
            stats.total_brews,
            settings.overshoot_delay_ms,
            settings.overshoot_ewma,
            settings.learning_confidence * 100.0,
            success_rate
        )
    }

    /// Reset all learning data (for debugging/testing)
    pub async fn reset_learning_data(&self) -> Result<(), Box<dyn std::error::Error>> {
        warn!("🔄 Resetting all learning data to defaults (MOCK MODE)");
        
        let mut settings = BrewSettings::default();
        settings.target_weight_g = self.get_settings().await.target_weight_g; // Preserve target weight
        
        let stats = BrewStatistics::default();
        
        self.update_settings(settings).await?;
        self.update_statistics(stats).await?;
        
        info!("✅ Learning data reset complete");
        Ok(())
    }
}