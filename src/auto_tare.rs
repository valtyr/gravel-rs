use crate::types::{AutoTareState, ScaleData, BrewState, TARE_STABILITY_THRESHOLD_G, TARE_STABILITY_COUNT, TARE_COOLDOWN_MS};
use embassy_time::{Duration, Instant};
use log::{info, debug};
use heapless::Vec;

pub struct AutoTareController {
    state: AutoTareState,
    weight_history: Vec<f32, 10>,
    last_tare_time: Option<Instant>,
    stability_count: usize,
    last_stable_weight: Option<f32>,
}

impl AutoTareController {
    pub fn new() -> Self {
        Self {
            state: AutoTareState::Empty,
            weight_history: Vec::new(),
            last_tare_time: None,
            stability_count: 0,
            last_stable_weight: None,
        }
    }
    
    pub fn update(&mut self, scale_data: &ScaleData, brew_state: BrewState) -> Option<TareAction> {
        if brew_state != BrewState::Idle {
            return None;
        }
        
        if let Some(last_tare) = self.last_tare_time {
            if Instant::now().duration_since(last_tare) < Duration::from_millis(TARE_COOLDOWN_MS) {
                return None;
            }
        }
        
        self.update_weight_history(scale_data.weight_g);
        
        let action = match self.state {
            AutoTareState::Empty => self.handle_empty_state(scale_data.weight_g),
            AutoTareState::Loading => self.handle_loading_state(scale_data.weight_g),
            AutoTareState::StableObject => self.handle_stable_object_state(scale_data.weight_g),
            AutoTareState::Unloading => self.handle_unloading_state(scale_data.weight_g),
        };
        
        debug!("AutoTare state: {:?}, weight: {:.2}g", self.state, scale_data.weight_g);
        action
    }
    
    fn update_weight_history(&mut self, weight: f32) {
        if self.weight_history.len() >= 10 {
            self.weight_history.remove(0);
        }
        let _ = self.weight_history.push(weight);
    }
    
    fn is_weight_stable(&self) -> bool {
        if self.weight_history.len() < TARE_STABILITY_COUNT {
            return false;
        }
        
        let recent_weights = &self.weight_history[self.weight_history.len() - TARE_STABILITY_COUNT..];
        let avg_weight: f32 = recent_weights.iter().sum::<f32>() / recent_weights.len() as f32;
        
        recent_weights.iter().all(|&w| (w - avg_weight).abs() < TARE_STABILITY_THRESHOLD_G)
    }
    
    fn get_stable_weight(&self) -> Option<f32> {
        if self.is_weight_stable() {
            let recent_weights = &self.weight_history[self.weight_history.len() - TARE_STABILITY_COUNT..];
            Some(recent_weights.iter().sum::<f32>() / recent_weights.len() as f32)
        } else {
            None
        }
    }
    
    fn handle_empty_state(&mut self, weight: f32) -> Option<TareAction> {
        if weight > 2.0 {
            info!("AutoTare: Detected object loading");
            self.state = AutoTareState::Loading;
            self.stability_count = 0;
            
            // Only trigger auto-tare if we've had a previous object that was removed
            // This prevents taring on the very first object placement
            if self.last_stable_weight.is_some() {
                info!("AutoTare: New object detected after previous removal - triggering tare");
                self.last_tare_time = Some(Instant::now());
                return Some(TareAction::Tare);
            }
        }
        None
    }
    
    fn handle_loading_state(&mut self, weight: f32) -> Option<TareAction> {
        if weight < 2.0 {
            info!("AutoTare: Object removed during loading");
            self.state = AutoTareState::Empty;
            return None;
        }
        
        if self.is_weight_stable() {
            if let Some(stable_weight) = self.get_stable_weight() {
                info!("AutoTare: Object stable at {:.2}g", stable_weight);
                self.state = AutoTareState::StableObject;
                self.last_stable_weight = Some(stable_weight);
            }
        }
        
        None
    }
    
    fn handle_stable_object_state(&mut self, weight: f32) -> Option<TareAction> {
        if let Some(stable_weight) = self.last_stable_weight {
            if (weight - stable_weight).abs() > 5.0 {
                if weight < stable_weight - 5.0 {
                    info!("AutoTare: Object being removed");
                    self.state = AutoTareState::Unloading;
                } else {
                    info!("AutoTare: Significant weight change, new object detected");
                    self.state = AutoTareState::Loading;
                    self.stability_count = 0;
                }
            }
        }
        None
    }
    
    fn handle_unloading_state(&mut self, weight: f32) -> Option<TareAction> {
        if weight < 2.0 {
            info!("AutoTare: Object completely removed - ready for new cup taring");
            self.state = AutoTareState::Empty;
            // Keep last_stable_weight to remember we had an object - needed for next auto-tare
        } else if weight > (self.last_stable_weight.unwrap_or(0.0) - 2.0) {
            info!("AutoTare: Object placed back");
            self.state = AutoTareState::StableObject;
        }
        None
    }
    
    pub fn get_state(&self) -> AutoTareState {
        self.state
    }
    
    pub fn reset(&mut self) {
        self.state = AutoTareState::Empty;
        self.weight_history.clear();
        self.stability_count = 0;
        self.last_stable_weight = None;
    }
}

#[derive(Debug, Clone, Copy)]
pub enum TareAction {
    Tare,
}