//! Scale event detection strategies
//! Analyzes raw scale data to detect user actions and state changes

use crate::system::events::{ScaleButton, ScaleEvent};
use crate::types::ScaleData;
use embassy_time::{Duration, Instant};
use log::{debug, info};

/// Threshold for detecting significant weight changes (grams)
const WEIGHT_CHANGE_THRESHOLD: f32 = 1.0;

/// Minimum time between button detections to avoid duplicates (ms)
const BUTTON_DEBOUNCE_MS: u64 = 500;

/// Timer state detection parameters
const TIMER_RESTART_THRESHOLD_MS: u32 = 100; // Timer restarted if timestamp jumps by less than this
const TIMER_STOP_THRESHOLD_MS: u32 = 30000; // Consider timer stopped if no change for this long

/// Object detection thresholds for auto-tare
const OBJECT_DETECTION_THRESHOLD: f32 = 5.0; // grams
const OBJECT_REMOVAL_THRESHOLD: f32 = 2.0; // grams

/// Strategy trait for detecting events from scale data
pub trait ScaleEventDetectionStrategy {
    /// Process new scale data and return detected events
    fn process_data(&mut self, data: &ScaleData) -> Vec<ScaleEvent>;
    
    /// Reset internal state
    fn reset(&mut self);
}

/// Historical data point for analysis
#[derive(Debug, Clone)]
struct DataPoint {
    data: ScaleData,
    timestamp: Instant,
}

/// Comprehensive scale event detector using multiple strategies
#[derive(Debug)]
pub struct ScaleEventDetector {
    // Historical data for analysis
    history: Vec<DataPoint>,
    max_history_size: usize,
    
    // Timer state tracking
    last_timer_timestamp: Option<u32>,
    timer_running: bool,
    last_timer_update: Option<Instant>,
    
    // Weight change tracking
    last_stable_weight: Option<f32>,
    weight_stable_since: Option<Instant>,
    last_weight_change: Option<Instant>,
    
    // Button detection
    last_button_detection: Option<Instant>,
    
    // Object detection state
    object_present: bool,
    last_object_change: Option<Instant>,
}

impl Default for ScaleEventDetector {
    fn default() -> Self {
        Self {
            history: Vec::new(),
            max_history_size: 50, // Keep last 5 seconds at 10Hz
            last_timer_timestamp: None,
            timer_running: false,
            last_timer_update: None,
            last_stable_weight: None,
            weight_stable_since: None,
            last_weight_change: None,
            last_button_detection: None,
            object_present: false,
            last_object_change: None,
        }
    }
}

impl ScaleEventDetector {
    pub fn new() -> Self {
        Self::default()
    }
    
    /// Process new scale data and detect events
    pub fn process_data(&mut self, data: &ScaleData) -> Vec<ScaleEvent> {
        let now = Instant::now();
        let mut events = Vec::new();
        
        // Add to history
        self.add_to_history(data.clone(), now);
        
        // Detect timer events
        events.extend(self.detect_timer_events(data, now));
        
        // Detect weight-based events
        events.extend(self.detect_weight_events(data, now));
        
        // Detect button presses (inferred from sudden timer changes)
        events.extend(self.detect_button_events(data, now));
        
        // Detect object placement/removal
        events.extend(self.detect_object_events(data, now));
        
        events
    }
    
    /// Add data point to history
    fn add_to_history(&mut self, data: ScaleData, timestamp: Instant) {
        self.history.push(DataPoint { data, timestamp });
        
        // Limit history size
        if self.history.len() > self.max_history_size {
            self.history.remove(0);
        }
    }
    
    /// Detect timer start/stop events
    fn detect_timer_events(&mut self, data: &ScaleData, now: Instant) -> Vec<ScaleEvent> {
        let mut events = Vec::new();
        
        // Check for timer state changes based on timestamp behavior
        if let Some(last_timestamp) = self.last_timer_timestamp {
            let timestamp_delta = if data.timestamp_ms > last_timestamp {
                data.timestamp_ms - last_timestamp
            } else {
                // Handle timestamp rollover or reset
                data.timestamp_ms
            };
            
            // Timer just started (timestamp jumped from 0 or small value)
            if !self.timer_running && data.timestamp_ms > 0 && timestamp_delta < TIMER_RESTART_THRESHOLD_MS {
                info!("⏱️ Timer started detected: {}ms", data.timestamp_ms);
                self.timer_running = true;
                self.last_timer_update = Some(now);
                events.push(ScaleEvent::TimerStarted { 
                    timestamp_ms: data.timestamp_ms 
                });
            }
            // Timer stopped (timestamp not increasing or went to 0)
            else if self.timer_running && (data.timestamp_ms == 0 || data.timestamp_ms == last_timestamp) {
                info!("⏹️ Timer stopped detected: {}ms", data.timestamp_ms);
                self.timer_running = false;
                events.push(ScaleEvent::TimerStopped { 
                    timestamp_ms: data.timestamp_ms 
                });
            }
            // Timer reset (timestamp jumped to small value)
            else if data.timestamp_ms < 1000 && last_timestamp > 5000 {
                info!("🔄 Timer reset detected: {}ms -> {}ms", last_timestamp, data.timestamp_ms);
                self.timer_running = data.timestamp_ms > 0;
                events.push(ScaleEvent::TimerReset);
                if self.timer_running {
                    events.push(ScaleEvent::TimerStarted { 
                        timestamp_ms: data.timestamp_ms 
                    });
                }
            }
        } else if data.timestamp_ms > 0 {
            // First timer start
            info!("⏱️ Initial timer start detected: {}ms", data.timestamp_ms);
            self.timer_running = true;
            self.last_timer_update = Some(now);
            events.push(ScaleEvent::TimerStarted { 
                timestamp_ms: data.timestamp_ms 
            });
        }
        
        // Check for timer timeout (no updates for a while)
        if self.timer_running {
            if let Some(last_update) = self.last_timer_update {
                if now.duration_since(last_update) > Duration::from_millis(TIMER_STOP_THRESHOLD_MS as u64) {
                    info!("⏰ Timer timeout detected - assuming stopped");
                    self.timer_running = false;
                    events.push(ScaleEvent::TimerStopped { 
                        timestamp_ms: data.timestamp_ms 
                    });
                }
            }
        }
        
        self.last_timer_timestamp = Some(data.timestamp_ms);
        if data.timestamp_ms > (self.last_timer_timestamp.unwrap_or(0)) {
            self.last_timer_update = Some(now);
        }
        
        events
    }
    
    /// Detect weight-based events (significant changes)
    fn detect_weight_events(&mut self, data: &ScaleData, now: Instant) -> Vec<ScaleEvent> {
        let mut events = Vec::new();
        
        // Check for significant weight changes
        if let Some(last_weight) = self.last_stable_weight {
            let weight_change = (data.weight_g - last_weight).abs();
            
            if weight_change > WEIGHT_CHANGE_THRESHOLD {
                debug!("📊 Significant weight change: {:.1}g -> {:.1}g (Δ{:.1}g)", 
                       last_weight, data.weight_g, data.weight_g - last_weight);
                       
                self.last_weight_change = Some(now);
                self.weight_stable_since = None; // Reset stability
                
                // Could trigger weight change event if needed
                // events.push(ScaleEvent::WeightChanged { data: data.clone() });
            } else {
                // Weight is stable
                if self.weight_stable_since.is_none() {
                    self.weight_stable_since = Some(now);
                } else if let Some(stable_since) = self.weight_stable_since {
                    // Update stable weight after 1 second of stability
                    if now.duration_since(stable_since) > Duration::from_millis(1000) {
                        self.last_stable_weight = Some(data.weight_g);
                    }
                }
            }
        } else {
            // First weight reading
            self.last_stable_weight = Some(data.weight_g);
            self.weight_stable_since = Some(now);
        }
        
        events
    }
    
    /// Detect button presses (inferred from data patterns)
    fn detect_button_events(&mut self, data: &ScaleData, now: Instant) -> Vec<ScaleEvent> {
        let mut events = Vec::new();
        
        // Debounce button detection
        if let Some(last_detection) = self.last_button_detection {
            if now.duration_since(last_detection) < Duration::from_millis(BUTTON_DEBOUNCE_MS) {
                return events;
            }
        }
        
        // Detect tare button (weight suddenly drops to near zero with little change in flow)
        if let Some(last_weight) = self.last_stable_weight {
            if last_weight > 5.0 && data.weight_g.abs() < 1.0 && data.flow_rate_g_per_s.abs() < 0.5 {
                info!("⚖️ Tare button detected: {:.1}g -> {:.1}g", last_weight, data.weight_g);
                self.last_button_detection = Some(now);
                events.push(ScaleEvent::ButtonPressed(ScaleButton::Tare));
            }
        }
        
        // Detect timer button (sudden timer state change without flow)
        if let Some(last_timestamp) = self.last_timer_timestamp {
            // Timer button pressed if timer state changed abruptly without significant flow
            if (data.timestamp_ms == 0 && last_timestamp > 1000) || 
               (data.timestamp_ms > 0 && last_timestamp == 0) {
                if data.flow_rate_g_per_s.abs() < 0.5 {
                    info!("⏲️ Timer button detected: {}ms -> {}ms", last_timestamp, data.timestamp_ms);
                    self.last_button_detection = Some(now);
                    events.push(ScaleEvent::ButtonPressed(ScaleButton::Timer));
                }
            }
        }
        
        events
    }
    
    /// Detect object placement/removal for auto-tare
    fn detect_object_events(&mut self, data: &ScaleData, now: Instant) -> Vec<ScaleEvent> {
        let mut events = Vec::new();
        
        // Debounce object detection
        if let Some(last_change) = self.last_object_change {
            if now.duration_since(last_change) < Duration::from_millis(1000) {
                return events;
            }
        }
        
        // Object placed (weight increased significantly)
        if !self.object_present && data.weight_g > OBJECT_DETECTION_THRESHOLD {
            info!("📦 Object detected: {:.1}g", data.weight_g);
            self.object_present = true;
            self.last_object_change = Some(now);
            // Note: We emit WeightChanged instead of custom ObjectDetected for now
            events.push(ScaleEvent::WeightChanged { data: data.clone() });
        }
        // Object removed (weight dropped significantly)
        else if self.object_present && data.weight_g < OBJECT_REMOVAL_THRESHOLD {
            info!("📤 Object removed: {:.1}g", data.weight_g);
            self.object_present = false;
            self.last_object_change = Some(now);
            events.push(ScaleEvent::WeightChanged { data: data.clone() });
        }
        
        events
    }
    
    /// Get recent history for analysis
    pub fn get_recent_history(&self, duration: Duration) -> Vec<&DataPoint> {
        let cutoff = Instant::now() - duration;
        self.history.iter()
            .filter(|point| point.timestamp > cutoff)
            .collect()
    }
    
    /// Reset all state
    pub fn reset(&mut self) {
        *self = Self::default();
    }
    
    /// Get current timer state
    pub fn is_timer_running(&self) -> bool {
        self.timer_running
    }
    
    /// Get current stable weight
    pub fn get_stable_weight(&self) -> Option<f32> {
        self.last_stable_weight
    }
}

impl ScaleEventDetectionStrategy for ScaleEventDetector {
    fn process_data(&mut self, data: &ScaleData) -> Vec<ScaleEvent> {
        self.process_data(data)
    }
    
    fn reset(&mut self) {
        self.reset()
    }
}

/// Timer-focused detection strategy
#[derive(Debug, Default)]
pub struct TimerDetectionStrategy {
    detector: ScaleEventDetector,
}

impl TimerDetectionStrategy {
    pub fn new() -> Self {
        Self::default()
    }
}

impl ScaleEventDetectionStrategy for TimerDetectionStrategy {
    fn process_data(&mut self, data: &ScaleData) -> Vec<ScaleEvent> {
        // Only return timer-related events
        self.detector.process_data(data)
            .into_iter()
            .filter(|event| matches!(event, 
                ScaleEvent::TimerStarted { .. } | 
                ScaleEvent::TimerStopped { .. } | 
                ScaleEvent::TimerReset))
            .collect()
    }
    
    fn reset(&mut self) {
        self.detector.reset();
    }
}

/// Button detection strategy
#[derive(Debug, Default)]
pub struct ButtonDetectionStrategy {
    detector: ScaleEventDetector,
}

impl ButtonDetectionStrategy {
    pub fn new() -> Self {
        Self::default()
    }
}

impl ScaleEventDetectionStrategy for ButtonDetectionStrategy {
    fn process_data(&mut self, data: &ScaleData) -> Vec<ScaleEvent> {
        // Only return button events
        self.detector.process_data(data)
            .into_iter()
            .filter(|event| matches!(event, ScaleEvent::ButtonPressed(_)))
            .collect()
    }
    
    fn reset(&mut self) {
        self.detector.reset();
    }
}

/// Weight change detection strategy
#[derive(Debug, Default)]
pub struct WeightDetectionStrategy {
    detector: ScaleEventDetector,
}

impl WeightDetectionStrategy {
    pub fn new() -> Self {
        Self::default()
    }
}

impl ScaleEventDetectionStrategy for WeightDetectionStrategy {
    fn process_data(&mut self, data: &ScaleData) -> Vec<ScaleEvent> {
        // Only return weight change events
        self.detector.process_data(data)
            .into_iter()
            .filter(|event| matches!(event, ScaleEvent::WeightChanged { .. }))
            .collect()
    }
    
    fn reset(&mut self) {
        self.detector.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use embassy_time::Duration;

    #[test]
    fn test_timer_detection() {
        let mut detector = ScaleEventDetector::new();
        
        // Timer start
        let data1 = ScaleData {
            timestamp_ms: 1000,
            weight_g: 0.0,
            flow_rate_g_per_s: 0.0,
            battery_percent: 100,
            timer_running: true,
            received_at: Instant::now(),
        };
        
        let events = detector.process_data(&data1);
        assert!(events.iter().any(|e| matches!(e, ScaleEvent::TimerStarted { .. })));
    }
    
    #[test]
    fn test_button_detection() {
        let mut detector = ScaleEventDetector::new();
        
        // Set initial weight
        let data1 = ScaleData {
            timestamp_ms: 0,
            weight_g: 20.0,
            flow_rate_g_per_s: 0.0,
            battery_percent: 100,
            timer_running: false,
            received_at: Instant::now(),
        };
        detector.process_data(&data1);
        
        // Wait for stability
        std::thread::sleep(std::time::Duration::from_millis(1100));
        
        // Tare button pressed (weight goes to zero)
        let data2 = ScaleData {
            timestamp_ms: 0,
            weight_g: 0.0,
            flow_rate_g_per_s: 0.0,
            battery_percent: 100,
            timer_running: false,
            received_at: Instant::now(),
        };
        
        let events = detector.process_data(&data2);
        assert!(events.iter().any(|e| matches!(e, ScaleEvent::ButtonPressed(ScaleButton::Tare))));
    }
}