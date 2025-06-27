use crate::types::{AutoTareState, ScaleData, BrewState, TARE_STABILITY_THRESHOLD_G, TARE_STABILITY_COUNT, TARE_COOLDOWN_MS};
use embassy_time::{Duration, Instant};
use log::{info, debug};
use heapless::Vec;

pub struct AutoTareController {
    enabled: bool,
    state: AutoTareState,
    stable_weight: f32,
    weight_history: Vec<f32, 10>,
    last_tare_time: Option<Instant>,
    empty_threshold: f32,
    stable_readings_needed: usize,
    brewing_cooldown_time: Option<Instant>, // Prevent auto-tare immediately after brewing
}

impl AutoTareController {
    pub fn new() -> Self {
        Self {
            enabled: true,  // Default enabled like Python
            state: AutoTareState::Empty,
            stable_weight: 0.0,
            weight_history: Vec::new(),
            last_tare_time: None,
            empty_threshold: 2.0,  // From Python
            stable_readings_needed: 5,  // From Python
            brewing_cooldown_time: None,
        }
    }
    
    /// Main auto-tare logic - called on every weight reading like Python
    pub fn should_auto_tare(&mut self, scale_data: &ScaleData, brew_state: BrewState, timer_running: bool) -> bool {
        if !self.enabled || timer_running || brew_state != BrewState::Idle {
            return false;
        }
        
        // Check brewing cooldown period (prevent auto-tare right after brewing)
        if let Some(brewing_cooldown) = self.brewing_cooldown_time {
            if Instant::now().duration_since(brewing_cooldown) < Duration::from_secs(10) {
                debug!("Auto-tare: Still in brewing cooldown period");
                return false;
            }
        }
        
        // Check regular tare cooldown period
        if let Some(last_tare) = self.last_tare_time {
            if Instant::now().duration_since(last_tare) < Duration::from_millis(TARE_COOLDOWN_MS) {
                return false;
            }
        }
        
        let current_weight = scale_data.weight_g;
        let is_stable = self.is_weight_stable(current_weight);
        let is_empty = current_weight.abs() <= self.empty_threshold;
        
        // State machine logic from Python
        match self.state {
            AutoTareState::Empty => {
                if !is_empty && is_stable {
                    // Object placed on empty scale - TARE IMMEDIATELY
                    self.state = AutoTareState::StableObject;
                    self.stable_weight = current_weight;
                    info!("AutoTare: Object detected: {:.1}g - TARING", current_weight);
                    return true;
                } else if !is_empty {
                    // Weight detected but not stable yet
                    self.state = AutoTareState::Loading;
                }
            }
            
            AutoTareState::Loading => {
                if is_stable {
                    if is_empty {
                        // Stabilized to empty
                        self.state = AutoTareState::Empty;
                        self.stable_weight = 0.0;
                    } else {
                        // Stabilized with object - TARE IMMEDIATELY
                        self.state = AutoTareState::StableObject;
                        self.stable_weight = current_weight;
                        info!("AutoTare: Object stabilized: {:.1}g - TARING", current_weight);
                        return true;
                    }
                }
            }
            
            AutoTareState::StableObject => {
                if is_empty && is_stable {
                    // Object removed - NO TARE, just go to Empty
                    self.state = AutoTareState::Empty;
                    self.stable_weight = 0.0;
                    info!("AutoTare: Object removed");
                } else if is_stable && (current_weight - self.stable_weight).abs() > 10.0 {
                    // MAJOR weight change - definitely cup swap (increased threshold to 10.0g for real-world use)
                    // Reset to Empty to force proper detection (NO IMMEDIATE TARE)
                    self.state = AutoTareState::Empty;
                    self.stable_weight = 0.0;
                    info!("AutoTare: Major cup change detected: {:.1}g -> {:.1}g", self.stable_weight, current_weight);
                } else if !is_stable {
                    // Weight changing - but only go to unloading if it's a significant change
                    // Small fluctuations after brewing shouldn't trigger unloading state
                    let recent_avg = if self.weight_history.len() >= 3 {
                        let recent: f32 = self.weight_history[self.weight_history.len() - 3..]
                            .iter().sum::<f32>() / 3.0;
                        recent
                    } else {
                        current_weight
                    };
                    
                    if (recent_avg - self.stable_weight).abs() > 5.0 {
                        info!("AutoTare: Major weight change detected, entering unloading state");
                        self.state = AutoTareState::Unloading;
                    }
                    // Otherwise stay in StableObject state for small fluctuations
                }
            }
            
            AutoTareState::Unloading => {
                if is_stable {
                    if is_empty {
                        // Removed completely
                        self.state = AutoTareState::Empty;
                        self.stable_weight = 0.0;
                        info!("AutoTare: Object removed");
                    } else {
                        // Stabilized at new weight - TARE IMMEDIATELY
                        self.state = AutoTareState::StableObject;
                        let old_weight = self.stable_weight;
                        self.stable_weight = current_weight;
                        info!("AutoTare: Object changed: {:.1}g → {:.1}g - TARING", old_weight, current_weight);
                        return true;
                    }
                }
            }
        }
        
        false
    }
    
    fn is_weight_stable(&mut self, current_weight: f32) -> bool {
        // Add to history
        if self.weight_history.len() >= 10 {
            self.weight_history.remove(0);
        }
        let _ = self.weight_history.push(current_weight);
        
        // Need at least stable_readings_needed readings
        if self.weight_history.len() < self.stable_readings_needed {
            return false;
        }
        
        // Use Python's simple min/max approach for consistent behavior
        let recent_weights = &self.weight_history[self.weight_history.len() - self.stable_readings_needed..];
        
        // Check if recent weights are within threshold of each other (Python method)
        let max_weight = recent_weights.iter().fold(f32::NEG_INFINITY, |a, &b| a.max(b));
        let min_weight = recent_weights.iter().fold(f32::INFINITY, |a, &b| a.min(b));
        
        // Consider stable if range is within threshold (exactly like Python)
        (max_weight - min_weight) <= TARE_STABILITY_THRESHOLD_G
    }
    
    pub fn record_tare(&mut self) {
        self.last_tare_time = Some(Instant::now());
    }
    
    pub fn get_state(&self) -> AutoTareState {
        self.state
    }
    
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }
    
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }
    
    pub fn reset(&mut self) {
        self.state = AutoTareState::Empty;
        self.weight_history.clear();
        self.stable_weight = 0.0;
    }
    
    /// Called when returning to idle after brewing - preserves current object state
    pub fn brewing_finished(&mut self, current_weight: f32) {
        // Set brewing cooldown to prevent auto-tare for 10 seconds after brewing
        self.brewing_cooldown_time = Some(Instant::now());
        
        // If we have a stable object after brewing, keep it as stable without re-taring
        if current_weight > self.empty_threshold {
            info!("AutoTare: Brewing finished, preserving object at {:.1}g (10s cooldown active)", current_weight);
            self.state = AutoTareState::StableObject;
            self.stable_weight = current_weight;
            // Clear weight history to rebuild stability for this object
            self.weight_history.clear();
        } else {
            info!("AutoTare: Brewing finished, scale empty");
            self.state = AutoTareState::Empty;
            self.stable_weight = 0.0;
        }
    }
}

// Remove the TareAction enum - we'll use bool directly like Python