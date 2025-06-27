use crate::types::{ScaleData, OVERSHOOT_HISTORY_SIZE};
use embassy_time::{Duration, Instant, Timer};
use log::{info, debug};
use heapless::Vec;

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
}

impl OvershootController {
    pub fn new() -> Self {
        let controller = Self {
            stop_delay_ms: 500, // Initial delay from Python
            overshoot_history: Vec::new(),
            pending_predicted_stop: false,
            max_history_size: 5,
        };
        
        let (min_time, max_time) = controller.calculate_prediction_window();
        info!("OvershootController initialized: delay={}ms, prediction_window=[{:.1}s, {:.1}s]", 
              controller.stop_delay_ms, min_time, max_time);
              
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
    
    /// Record overshoot and adjust delay if flow has stopped (from Python)
    pub fn record_overshoot(&mut self, actual_weight: f32, target_weight: f32, flow_rate: f32) {
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
        
        info!("Recording overshoot: flow stopped after predicted stop");
        self.pending_predicted_stop = false;
        let overshoot = actual_weight - target_weight;
        
        let measurement = OvershootMeasurement {
            overshoot,
            timestamp: Instant::now(),
        };
        
        // Keep only recent measurements
        if self.overshoot_history.len() >= self.max_history_size {
            self.overshoot_history.remove(0);
        }
        let _ = self.overshoot_history.push(measurement);
        
        // Adjust delay based on average overshoot
        let avg_overshoot = self.overshoot_history.iter()
            .map(|m| m.overshoot)
            .sum::<f32>() / self.overshoot_history.len() as f32;
        
        if avg_overshoot > 1.0 {
            // Overshooting - stop earlier
            self.stop_delay_ms += 100;
        } else if avg_overshoot < -1.0 {
            // Undershooting - stop later
            self.stop_delay_ms = (self.stop_delay_ms - 100).max(100);
        }
        
        info!("📊 Final overshoot: {:.1}g, Avg: {:.1}g, New delay: {}ms",
              overshoot, avg_overshoot, self.stop_delay_ms);
    }
    
    pub fn get_current_delay_ms(&self) -> i32 {
        self.stop_delay_ms
    }
    
    pub fn reset(&mut self) {
        info!("Resetting overshoot controller");
        self.overshoot_history.clear();
        self.stop_delay_ms = 500;
        self.pending_predicted_stop = false;
    }
}

// Simple structure for stop timing information
#[derive(Debug, Clone)]
pub struct StopTiming {
    pub should_stop_now: bool,
    pub delay_ms: i32,
    pub predicted_final_weight: f32,
}