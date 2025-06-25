use crate::types::{ScaleData, OVERSHOOT_HISTORY_SIZE, PREDICTION_SAFETY_MARGIN_G};
use embassy_time::{Duration, Instant};
use log::{info, debug};
use heapless::Vec;

#[derive(Debug, Clone)]
struct OvershootMeasurement {
    predicted_stop_weight: f32,
    actual_final_weight: f32,
    overshoot: f32,
    timestamp: Instant,
}

pub struct OvershootController {
    overshoot_history: Vec<OvershootMeasurement, OVERSHOOT_HISTORY_SIZE>,
    current_stop_delay_ms: i32,
    last_prediction: Option<PredictionData>,
    learning_enabled: bool,
}

#[derive(Debug, Clone)]
struct PredictionData {
    predicted_weight: f32,
    prediction_time: Instant,
    flow_rate: f32,
}

impl OvershootController {
    pub fn new() -> Self {
        Self {
            overshoot_history: Vec::new(),
            current_stop_delay_ms: 0,
            last_prediction: None,
            learning_enabled: true,
        }
    }
    
    pub fn calculate_stop_timing(&mut self, 
                                 current_weight: f32, 
                                 target_weight: f32, 
                                 flow_rate: f32) -> Option<StopTiming> {
        if flow_rate <= 0.0 {
            return None;
        }
        
        let weight_to_target = target_weight - current_weight;
        if weight_to_target <= 0.0 {
            return Some(StopTiming {
                should_stop_now: true,
                delay_ms: 0,
                predicted_final_weight: current_weight,
            });
        }
        
        let base_time_to_target_ms = (weight_to_target / flow_rate * 1000.0) as i32;
        
        let adjusted_delay = base_time_to_target_ms + self.current_stop_delay_ms;
        
        let predicted_final_weight = current_weight + (flow_rate * (adjusted_delay as f32 / 1000.0));
        
        let should_stop_now = adjusted_delay <= 0 || 
                             predicted_final_weight >= (target_weight - PREDICTION_SAFETY_MARGIN_G);
        
        self.last_prediction = Some(PredictionData {
            predicted_weight: predicted_final_weight,
            prediction_time: Instant::now(),
            flow_rate,
        });
        
        debug!("Stop timing: weight={:.2}g, target={:.2}g, flow={:.2}g/s, delay={}ms, predicted={:.2}g",
               current_weight, target_weight, flow_rate, adjusted_delay, predicted_final_weight);
        
        Some(StopTiming {
            should_stop_now,
            delay_ms: adjusted_delay.max(0),
            predicted_final_weight,
        })
    }
    
    pub fn record_actual_result(&mut self, final_weight: f32) {
        if let Some(prediction) = &self.last_prediction {
            let overshoot = final_weight - prediction.predicted_weight;
            
            let measurement = OvershootMeasurement {
                predicted_stop_weight: prediction.predicted_weight,
                actual_final_weight: final_weight,
                overshoot,
                timestamp: Instant::now(),
            };
            
            info!("Overshoot measurement: predicted={:.2}g, actual={:.2}g, overshoot={:.2}g",
                  prediction.predicted_weight, final_weight, overshoot);
            
            if self.overshoot_history.len() >= OVERSHOOT_HISTORY_SIZE {
                self.overshoot_history.remove(0);
            }
            let _ = self.overshoot_history.push(measurement);
            
            if self.learning_enabled {
                self.update_delay_compensation();
            }
        }
        
        self.last_prediction = None;
    }
    
    fn update_delay_compensation(&mut self) {
        if self.overshoot_history.len() < 2 {
            return;
        }
        
        let total_overshoot: f32 = self.overshoot_history.iter()
            .map(|m| m.overshoot)
            .sum();
        let avg_overshoot = total_overshoot / self.overshoot_history.len() as f32;
        
        const LEARNING_RATE: f32 = 0.5;
        const MAX_ADJUSTMENT_MS: i32 = 100;
        
        let adjustment = (avg_overshoot * LEARNING_RATE * 1000.0) as i32;
        let clamped_adjustment = adjustment.max(-MAX_ADJUSTMENT_MS).min(MAX_ADJUSTMENT_MS);
        
        let old_delay = self.current_stop_delay_ms;
        self.current_stop_delay_ms -= clamped_adjustment;
        
        self.current_stop_delay_ms = self.current_stop_delay_ms.max(-500).min(500);
        
        if old_delay != self.current_stop_delay_ms {
            info!("Updated stop delay: {}ms -> {}ms (avg overshoot: {:.2}g)",
                  old_delay, self.current_stop_delay_ms, avg_overshoot);
        }
    }
    
    pub fn get_current_delay_ms(&self) -> i32 {
        self.current_stop_delay_ms
    }
    
    pub fn get_average_overshoot(&self) -> Option<f32> {
        if self.overshoot_history.is_empty() {
            return None;
        }
        
        let total: f32 = self.overshoot_history.iter()
            .map(|m| m.overshoot)
            .sum();
        Some(total / self.overshoot_history.len() as f32)
    }
    
    pub fn reset(&mut self) {
        info!("Resetting overshoot controller");
        self.overshoot_history.clear();
        self.current_stop_delay_ms = 0;
        self.last_prediction = None;
    }
    
    pub fn set_learning_enabled(&mut self, enabled: bool) {
        info!("Overshoot learning: {}", if enabled { "enabled" } else { "disabled" });
        self.learning_enabled = enabled;
    }
    
    pub fn get_overshoot_stats(&self) -> OvershootStats {
        if self.overshoot_history.is_empty() {
            return OvershootStats {
                count: 0,
                average: 0.0,
                min: 0.0,
                max: 0.0,
                standard_deviation: 0.0,
            };
        }
        
        let overshoots: Vec<f32, OVERSHOOT_HISTORY_SIZE> = self.overshoot_history.iter()
            .map(|m| m.overshoot)
            .collect();
        
        let average = overshoots.iter().sum::<f32>() / overshoots.len() as f32;
        let min = overshoots.iter().fold(f32::INFINITY, |a, &b| a.min(b));
        let max = overshoots.iter().fold(f32::NEG_INFINITY, |a, &b| a.max(b));
        
        let variance = overshoots.iter()
            .map(|&x| (x - average).powi(2))
            .sum::<f32>() / overshoots.len() as f32;
        let standard_deviation = variance.sqrt();
        
        OvershootStats {
            count: overshoots.len(),
            average,
            min,
            max,
            standard_deviation,
        }
    }
}

#[derive(Debug, Clone)]
pub struct StopTiming {
    pub should_stop_now: bool,
    pub delay_ms: i32,
    pub predicted_final_weight: f32,
}

#[derive(Debug, Clone)]
pub struct OvershootStats {
    pub count: usize,
    pub average: f32,
    pub min: f32,
    pub max: f32,
    pub standard_deviation: f32,
}