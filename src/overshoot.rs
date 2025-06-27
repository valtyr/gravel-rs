use crate::types::{ScaleData, OVERSHOOT_HISTORY_SIZE};
use crate::nvs_storage::NvsStorage;
use embassy_time::{Duration, Instant, Timer};
use log::{info, debug, warn};
use heapless::Vec;
use std::sync::Arc;

#[derive(Debug, Clone)]
struct OvershootMeasurement {
    overshoot: f32,
    timestamp: Instant,
}

pub struct OvershootController {
    stop_delay_ms: i32,
    overshoot_history: Vec<OvershootMeasurement, OVERSHOOT_HISTORY_SIZE>,
    pending_predicted_stop: bool,
    max_history_size: usize,
    
    // Smart learning algorithm
    ewma_overshoot: f32,        // Exponentially weighted moving average
    learning_rate: f32,         // Adaptive learning rate (0.1 to 0.5)
    confidence_score: f32,      // Learning confidence (0.0 to 1.0)
    brew_count: u32,           // Total brews for confidence calculation
    
    // NVS persistence
    nvs_storage: Option<Arc<NvsStorage>>,
}

impl OvershootController {
    pub fn new() -> Self {
        let controller = Self {
            stop_delay_ms: 500, // Initial delay from Python
            overshoot_history: Vec::new(),
            pending_predicted_stop: false,
            max_history_size: 5,
            
            // Smart learning defaults
            ewma_overshoot: 0.0,
            learning_rate: 0.3,     // 30% new data, 70% historical
            confidence_score: 0.0,
            brew_count: 0,
            
            nvs_storage: None,
        };
        
        let (min_time, max_time) = controller.calculate_prediction_window();
        info!("OvershootController initialized: delay={}ms, ewma={:.1}g, prediction_window=[{:.1}s, {:.1}s]", 
              controller.stop_delay_ms, controller.ewma_overshoot, min_time, max_time);
              
        controller
    }
    
    /// Initialize with NVS storage and load saved learning data
    pub async fn new_with_nvs(nvs_storage: Arc<NvsStorage>) -> Self {
        let mut controller = Self::new();
        controller.nvs_storage = Some(nvs_storage.clone());
        
        // Load saved learning data
        let settings = nvs_storage.get_settings().await;
        let stats = nvs_storage.get_statistics().await;
        
        controller.stop_delay_ms = settings.overshoot_delay_ms;
        controller.ewma_overshoot = settings.overshoot_ewma;
        controller.confidence_score = settings.learning_confidence;
        controller.brew_count = stats.total_brews;
        
        // Adaptive learning rate based on confidence
        controller.learning_rate = if controller.confidence_score > 0.8 {
            0.1  // Slow learning when confident
        } else if controller.confidence_score > 0.5 {
            0.2  // Medium learning
        } else {
            0.3  // Fast learning when uncertain
        };
        
        info!("OvershootController loaded from NVS: delay={}ms, ewma={:.1}g, confidence={:.1}%, brews={}", 
              controller.stop_delay_ms, controller.ewma_overshoot, 
              controller.confidence_score * 100.0, controller.brew_count);
              
        controller
    }
    
    /// Calculate valid prediction time window based on learned delay (from Python)
    pub fn calculate_prediction_window(&self) -> (f32, f32) {
        let min_reaction_time = (self.stop_delay_ms as f32 / 1000.0) + 0.2; // delay + safety margin
        let max_prediction_time = min_reaction_time * 3.0; // Don't predict too far ahead
        (min_reaction_time, max_prediction_time)
    }
    
    /// Get delay with overshoot compensation applied (from Python)
    pub fn get_compensated_delay(&self, target_delay: f32) -> f32 {
        (target_delay - (self.stop_delay_ms as f32 / 1000.0)).max(0.1)
    }
    
    /// Mark that a predicted stop was initiated (from Python)
    pub fn mark_predicted_stop(&mut self) {
        self.pending_predicted_stop = true;
    }
    
    /// Record overshoot and adjust delay using smart EWMA algorithm
    pub async fn record_overshoot(&mut self, actual_weight: f32, target_weight: f32, flow_rate: f32) {
        debug!("Overshoot check: pending={}, flow={:.2}g/s, weight={:.2}g, target={:.2}g", 
               self.pending_predicted_stop, flow_rate, actual_weight, target_weight);
               
        if !self.pending_predicted_stop {
            debug!("Overshoot: No pending predicted stop - skipping");
            return;
        }
        
        if flow_rate.abs() >= 0.5 {
            debug!("Overshoot: Flow rate {:.2}g/s >= 0.5g/s - waiting for flow to stop", flow_rate);
            return;
        }
        
        info!("🎯 Recording overshoot: flow stopped after predicted stop");
        self.pending_predicted_stop = false;
        let overshoot = actual_weight - target_weight;
        
        // Add to history for legacy compatibility
        let measurement = OvershootMeasurement {
            overshoot,
            timestamp: Instant::now(),
        };
        if self.overshoot_history.len() >= self.max_history_size {
            self.overshoot_history.remove(0);
        }
        let _ = self.overshoot_history.push(measurement);
        
        // Smart EWMA learning algorithm
        self.update_ewma_and_delay(overshoot).await;
        
        // Update brew count and confidence
        self.brew_count += 1;
        self.update_confidence();
        
        // Save to NVS if available
        if let Some(ref nvs) = self.nvs_storage {
            if let Err(e) = nvs.update_overshoot_learning(
                self.stop_delay_ms, 
                self.ewma_overshoot, 
                self.confidence_score
            ).await {
                warn!("Failed to save overshoot learning to NVS: {:?}", e);
            }
            
            // Record brew statistics
            nvs.record_brew(overshoot, true, overshoot.abs() <= 1.0).await;
        } else {
            debug!("NVS not available - overshoot learning not persisted");
        }
        
        info!("📊 Smart overshoot learning: {:.1}g -> ewma={:.1}g, delay={}ms, confidence={:.1}%, brews={}",
              overshoot, self.ewma_overshoot, self.stop_delay_ms, 
              self.confidence_score * 100.0, self.brew_count);
    }
    
    /// Update EWMA and calculate new delay using proportional control
    async fn update_ewma_and_delay(&mut self, new_overshoot: f32) {
        // Update EWMA with adaptive learning rate
        let old_ewma = self.ewma_overshoot;
        self.ewma_overshoot = self.learning_rate * new_overshoot + (1.0 - self.learning_rate) * old_ewma;
        
        debug!("EWMA update: {:.1}g + {:.1}g -> {:.1}g (rate={:.1}%)", 
               old_ewma, new_overshoot, self.ewma_overshoot, self.learning_rate * 100.0);
        
        // Proportional control: larger errors = bigger adjustments
        let error_magnitude = self.ewma_overshoot.abs();
        let base_adjustment = (error_magnitude * 50.0).min(200.0).max(10.0); // 10-200ms range
        
        // Confidence modifier: less confident = smaller adjustments
        let confidence_modifier = (self.confidence_score * 0.5 + 0.5).min(1.0); // 0.5 to 1.0 range
        let adjustment = (base_adjustment * confidence_modifier) as i32;
        
        let old_delay = self.stop_delay_ms;
        
        if self.ewma_overshoot > 0.5 {
            // Overshooting - stop earlier (increase delay)
            self.stop_delay_ms += adjustment;
            self.stop_delay_ms = self.stop_delay_ms.min(2000); // Cap at 2 seconds
            info!("🔼 Overshooting by {:.1}g, increasing delay by {}ms: {}ms -> {}ms", 
                  self.ewma_overshoot, adjustment, old_delay, self.stop_delay_ms);
        } else if self.ewma_overshoot < -0.5 {
            // Undershooting - stop later (decrease delay)
            self.stop_delay_ms -= adjustment;
            self.stop_delay_ms = self.stop_delay_ms.max(100); // Minimum 100ms
            info!("🔽 Undershooting by {:.1}g, decreasing delay by {}ms: {}ms -> {}ms", 
                  self.ewma_overshoot.abs(), adjustment, old_delay, self.stop_delay_ms);
        } else {
            debug!("✅ EWMA within ±0.5g threshold, keeping delay at {}ms", self.stop_delay_ms);
        }
    }
    
    /// Update learning confidence based on consistency
    fn update_confidence(&mut self) {
        if self.overshoot_history.len() < 3 {
            self.confidence_score = 0.0;
            return;
        }
        
        // Calculate consistency (lower variance = higher confidence)
        let mut recent_overshoots = heapless::Vec::<f32, OVERSHOOT_HISTORY_SIZE>::new();
        for measurement in self.overshoot_history.iter() {
            let _ = recent_overshoots.push(measurement.overshoot);
        }
        
        let mean: f32 = recent_overshoots.iter().sum::<f32>() / recent_overshoots.len() as f32;
        let variance: f32 = recent_overshoots.iter()
            .map(|&x| (x - mean).powi(2))
            .sum::<f32>() / recent_overshoots.len() as f32;
        
        let std_dev = variance.sqrt();
        
        // Convert consistency to confidence (lower std_dev = higher confidence)
        // Max confidence when std_dev <= 0.5g, min confidence when std_dev >= 3.0g
        let consistency_score = (3.0f32 - std_dev).max(0.0) / 2.5; // 0.0 to 1.0
        
        // Experience factor: more brews = higher confidence
        let experience_factor = (self.brew_count as f32 / 20.0).min(1.0); // Max confidence at 20 brews
        
        // Combined confidence
        self.confidence_score = (consistency_score * experience_factor).min(1.0);
        
        debug!("Confidence update: consistency={:.2}, experience={:.2}, combined={:.2}", 
               consistency_score, experience_factor, self.confidence_score);
    }
    
    pub fn get_current_delay_ms(&self) -> i32 {
        self.stop_delay_ms
    }
    
    /// Get detailed learning information for logging/debugging
    pub fn get_learning_info(&self) -> String {
        format!(
            "Learning: delay={}ms, ewma={:.1}g, confidence={:.1}%, brews={}, ready={}",
            self.stop_delay_ms,
            self.ewma_overshoot,
            self.confidence_score * 100.0,
            self.brew_count,
            self.is_learning_ready()
        )
    }
    
    pub async fn reset(&mut self) {
        info!("🔄 Resetting overshoot controller to defaults");
        self.overshoot_history.clear();
        self.stop_delay_ms = 500;
        self.pending_predicted_stop = false;
        self.ewma_overshoot = 0.0;
        self.confidence_score = 0.0;
        self.brew_count = 0;
        self.learning_rate = 0.3;
        
        // Reset NVS data if available
        if let Some(ref nvs) = self.nvs_storage {
            if let Err(e) = nvs.reset_learning_data().await {
                warn!("Failed to reset NVS learning data: {:?}", e);
            }
        } else {
            debug!("NVS not available - no persistent data to reset");
        }
    }
    
    /// Get learning statistics for display
    pub fn get_learning_stats(&self) -> (f32, f32, u32) {
        (self.ewma_overshoot, self.confidence_score, self.brew_count)
    }
    
    /// Check if the learning algorithm is ready (has enough data)
    pub fn is_learning_ready(&self) -> bool {
        self.brew_count >= 3 && self.confidence_score > 0.2
    }
}

// Simple structure for stop timing information
#[derive(Debug, Clone)]
pub struct StopTiming {
    pub should_stop_now: bool,
    pub delay_ms: i32,
    pub predicted_final_weight: f32,
}

