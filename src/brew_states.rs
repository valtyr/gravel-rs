use crate::types::{BrewState, ScaleData, TimerState, BREW_SETTLING_TIMEOUT_MS};
use embassy_time::{Duration, Instant};
use log::{info, debug};

pub struct BrewStateMachine {
    state: BrewState,
    settle_start_time: Option<Instant>,
    last_weight: Option<f32>,
}

impl BrewStateMachine {
    pub fn new() -> Self {
        Self {
            state: BrewState::Idle,
            settle_start_time: None,
            last_weight: None,
        }
    }
    
    pub fn update(&mut self, scale_data: &ScaleData, timer_state: TimerState) -> Option<BrewStateTransition> {
        let previous_state = self.state;
        
        match self.state {
            BrewState::Idle => self.handle_idle_state(scale_data, timer_state),
            BrewState::Brewing => self.handle_brewing_state(scale_data, timer_state),
            BrewState::BrewSettling => self.handle_settling_state(scale_data, timer_state),
        }
        
        if self.state != previous_state {
            info!("BrewState transition: {:?} -> {:?}", previous_state, self.state);
            Some(BrewStateTransition {
                from: previous_state,
                to: self.state,
            })
        } else {
            None
        }
    }
    
    fn handle_idle_state(&mut self, scale_data: &ScaleData, timer_state: TimerState) {
        if timer_state == TimerState::Running {
            info!("Timer started, entering brewing state");
            self.state = BrewState::Brewing;
            self.last_weight = Some(scale_data.weight_g);
        }
    }
    
    fn handle_brewing_state(&mut self, scale_data: &ScaleData, timer_state: TimerState) {
        if timer_state != TimerState::Running {
            info!("Timer stopped, entering settling state");
            self.state = BrewState::BrewSettling;
            self.settle_start_time = Some(Instant::now());
        }
        
        self.last_weight = Some(scale_data.weight_g);
    }
    
    fn handle_settling_state(&mut self, scale_data: &ScaleData, timer_state: TimerState) {
        if timer_state == TimerState::Running {
            info!("Timer restarted during settling, back to brewing");
            self.state = BrewState::Brewing;
            self.settle_start_time = None;
            return;
        }
        
        if let Some(settle_start) = self.settle_start_time {
            let settle_duration = Instant::now().duration_since(settle_start);
            
            if settle_duration > Duration::from_millis(BREW_SETTLING_TIMEOUT_MS) {
                info!("Settling timeout reached, returning to idle");
                self.state = BrewState::Idle;
                self.settle_start_time = None;
                return;
            }
        }
        
        if let Some(last_weight) = self.last_weight {
            let weight_change = (scale_data.weight_g - last_weight).abs();
            
            if weight_change > 10.0 {
                info!("Significant weight change during settling ({}g), cup likely removed", weight_change);
                self.state = BrewState::Idle;
                self.settle_start_time = None;
            }
        }
        
        self.last_weight = Some(scale_data.weight_g);
    }
    
    pub fn get_state(&self) -> BrewState {
        self.state
    }
    
    pub fn force_idle(&mut self) {
        info!("Forcing brew state to idle");
        self.state = BrewState::Idle;
        self.settle_start_time = None;
        self.last_weight = None;
    }
    
    pub fn get_settling_progress(&self) -> Option<f32> {
        if self.state == BrewState::BrewSettling {
            if let Some(settle_start) = self.settle_start_time {
                let elapsed = Instant::now().duration_since(settle_start);
                let progress = elapsed.as_millis() as f32 / BREW_SETTLING_TIMEOUT_MS as f32;
                Some(progress.min(1.0))
            } else {
                None
            }
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct BrewStateTransition {
    pub from: BrewState,
    pub to: BrewState,
}