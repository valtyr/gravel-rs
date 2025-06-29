//! World-class event bus for the espresso controller
//! Clean, type-safe interface hiding embassy-sync complexity

use crate::types::{BrewState, ScaleData};
use crate::scales::traits::{ScaleInfo, ScaleCommand as TraitScaleCommand};
use embassy_sync::{
    blocking_mutex::raw::CriticalSectionRawMutex,
    pubsub::{PubSubChannel, Publisher, Subscriber},
};
use embassy_time::{Duration, Instant};
use std::sync::Arc;

// === COMPREHENSIVE EVENT HIERARCHY ===

/// Top-level system event - everything flows through this
#[derive(Debug, Clone)]
pub enum SystemEvent {
    Scale(ScaleEvent),
    Brew(BrewEvent), 
    User(UserEvent),
    Time(TimeEvent),
    Safety(SafetyEvent),
    Hardware(HardwareEvent),
    Network(NetworkEvent),
}

/// Scale-related events (from hardware or inferred)
#[derive(Debug, Clone)]
pub enum ScaleEvent {
    // Raw data
    WeightChanged { data: ScaleData },
    Connected { info: ScaleInfo },
    Disconnected { reason: String },
    
    // Inferred user actions (from ScaleEventDetector strategies)
    ButtonPressed(ScaleButton),
    
    // Timer events (detected from scale data)
    TimerStarted { timestamp_ms: u32 },
    TimerStopped { timestamp_ms: u32 },
    TimerReset,
}

/// Scale button types (inferred or explicit)
#[derive(Debug, Clone, Copy)]
pub enum ScaleButton {
    Tare,
    Timer,     // Start/stop/reset cycle
    Power,
    Mode,
}

/// Brewing process events
#[derive(Debug, Clone)]
pub enum BrewEvent {
    // State machine transitions
    StateChanged { from: BrewState, to: BrewState },
    
    // Brewing milestones
    Started { target_weight: f32 },
    TargetWeightReached { actual: f32, target: f32 },
    PredictiveStopTriggered { predicted_overshoot: f32 },
    Finished { final_weight: f32, duration_ms: u32 },
    
    // Auto-tare events
    AutoTareTriggered { reason: String },
    ObjectDetected { weight: f32 },
    ObjectRemoved,
}

/// User-initiated events (web interface, future physical buttons)
#[derive(Debug, Clone)]
pub enum UserEvent {
    // Configuration changes
    SetTargetWeight(f32),
    SetAutoTare(bool),
    SetPredictiveStop(bool),
    
    // Manual actions
    TareScale,
    StartBrewing,
    StopBrewing,
    ResetTimer,
    TestRelay,
    ResetOvershoot,
    
    // WiFi provisioning
    StartWifiProvisioning,
    ResetWifiCredentials,
    
    // System control
    EmergencyStop,
    RebootSystem,
}

/// Time-based events for state machine ticks
#[derive(Debug, Clone)]
pub enum TimeEvent {
    Tick,                              // Regular 100ms tick
    Timeout { id: String },            // Named timeout expired
    SettlingTimeout,                   // Brew settling period over
    AutoTareDelay,                     // Auto-tare cooldown expired
    PredictiveStopDelay { delay_ms: u32 }, // Predictive stop execution
}

/// Safety and error events
#[derive(Debug, Clone)]
pub enum SafetyEvent {
    EmergencyStop { reason: String },
    DataTimeout { source: String },    // No data from scale/network
    RelayStuck { state: bool },        // Relay failed to change state
    WatchdogTriggered,
    OverTemperature,
    SystemAlert { level: AlertLevel, message: String },
}

#[derive(Debug, Clone, Copy)]
pub enum AlertLevel {
    Info,
    Warning, 
    Error,
    Critical,
}

impl AlertLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            AlertLevel::Info => "INFO",
            AlertLevel::Warning => "WARNING",
            AlertLevel::Error => "ERROR", 
            AlertLevel::Critical => "CRITICAL",
        }
    }
}

/// Hardware control events (pure side effects)
#[derive(Debug, Clone)]
pub enum HardwareEvent {
    // Relay control
    RelayOn,
    RelayOff,
    
    // Scale commands
    SendScaleCommand(ScaleCommand),
    
    // Display updates
    DisplayUpdate { state: DisplayState },
    DisplayAlert { message: String, duration: Duration },
}

// Re-export the traits ScaleCommand to avoid duplication
pub use crate::scales::traits::ScaleCommand;

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

/// Network and connectivity events
#[derive(Debug, Clone)]
pub enum NetworkEvent {
    WifiConnected { ssid: String },
    WifiDisconnected,
    BleConnected { device_name: String },
    BleDisconnected,
    WebSocketClientConnected,
    WebSocketClientDisconnected,
    ProvisioningStarted,
    ProvisioningCompleted,
}

// === CLEAN EVENT BUS INTERFACE ===

/// World-class event bus with clean, type-safe interface
/// Hides embassy-sync complexity behind simple publish/subscribe API
pub struct EventBus {
    // Single channel for all system events
    channel: PubSubChannel<CriticalSectionRawMutex, SystemEvent, 64, 8, 8>,
}

impl EventBus {
    pub fn new() -> Self {
        Self {
            channel: PubSubChannel::new(),
        }
    }

    /// Get a publisher handle - clean interface
    pub fn publisher(&self) -> EventPublisher {
        EventPublisher {
            inner: self.channel.publisher().unwrap(),
        }
    }

    /// Get a filtered subscriber - clean interface with type safety
    pub fn subscriber(&self) -> EventSubscriber {
        EventSubscriber {
            inner: self.channel.subscriber().unwrap(),
        }
    }

    /// Get a subscriber filtered to specific event types
    pub fn filtered_subscriber<F>(&self, filter: F) -> FilteredEventSubscriber<F>
    where
        F: Fn(&SystemEvent) -> bool + Send + Sync,
    {
        FilteredEventSubscriber {
            inner: self.channel.subscriber().unwrap(),
            filter,
        }
    }

    /// Convenience method: subscribe only to scale events
    pub fn scale_events_subscriber(&self) -> FilteredEventSubscriber<impl Fn(&SystemEvent) -> bool> {
        self.filtered_subscriber(|event| matches!(event, SystemEvent::Scale(_)))
    }

    /// Convenience method: subscribe only to brew events  
    pub fn brew_events_subscriber(&self) -> FilteredEventSubscriber<impl Fn(&SystemEvent) -> bool> {
        self.filtered_subscriber(|event| matches!(event, SystemEvent::Brew(_)))
    }

    /// Convenience method: subscribe only to user events
    pub fn user_events_subscriber(&self) -> FilteredEventSubscriber<impl Fn(&SystemEvent) -> bool> {
        self.filtered_subscriber(|event| matches!(event, SystemEvent::User(_)))
    }

    /// Convenience method: subscribe to hardware events (for side effect handlers)
    pub fn hardware_events_subscriber(&self) -> FilteredEventSubscriber<impl Fn(&SystemEvent) -> bool> {
        self.filtered_subscriber(|event| matches!(event, SystemEvent::Hardware(_)))
    }
}

/// Clean publisher interface - no exposed embassy types
pub struct EventPublisher<'a> {
    inner: Publisher<'a, CriticalSectionRawMutex, SystemEvent, 64, 8, 8>,
}

impl<'a> EventPublisher<'a> {
    /// Publish any system event - single clean interface
    pub async fn publish(&self, event: SystemEvent) {
        self.inner.publish(event).await;
    }

    /// Convenience methods for common events
    pub async fn scale_weight_changed(&self, data: ScaleData) {
        self.publish(SystemEvent::Scale(ScaleEvent::WeightChanged { data })).await;
    }

    pub async fn scale_connected(&self, info: ScaleInfo) {
        self.publish(SystemEvent::Scale(ScaleEvent::Connected { info })).await;
    }

    pub async fn user_command(&self, command: UserEvent) {
        self.publish(SystemEvent::User(command)).await;
    }

    pub async fn emergency_stop(&self, reason: String) {
        self.publish(SystemEvent::Safety(SafetyEvent::EmergencyStop { reason })).await;
    }

    pub async fn relay_on(&self) {
        self.publish(SystemEvent::Hardware(HardwareEvent::RelayOn)).await;
    }

    pub async fn relay_off(&self) {
        self.publish(SystemEvent::Hardware(HardwareEvent::RelayOff)).await;
    }
}

/// Clean subscriber interface
pub struct EventSubscriber<'a> {
    inner: Subscriber<'a, CriticalSectionRawMutex, SystemEvent, 64, 8, 8>,
}

impl<'a> EventSubscriber<'a> {
    /// Wait for any system event
    pub async fn next_event(&mut self) -> SystemEvent {
        loop {
            match self.inner.next_message().await {
                embassy_sync::pubsub::WaitResult::Lagged(_count) => {
                    // If lagged, continue to next iteration
                    continue;
                }
                embassy_sync::pubsub::WaitResult::Message(event) => return event,
            }
        }
    }
}

/// Filtered subscriber - only receives events matching the filter
pub struct FilteredEventSubscriber<'a, F>
where
    F: Fn(&SystemEvent) -> bool + Send + Sync,
{
    inner: Subscriber<'a, CriticalSectionRawMutex, SystemEvent, 64, 8, 8>,
    filter: F,
}

impl<'a, F> FilteredEventSubscriber<'a, F>
where
    F: Fn(&SystemEvent) -> bool + Send + Sync,
{
    /// Wait for next event matching the filter
    pub async fn next_event(&mut self) -> SystemEvent {
        loop {
            let event = match self.inner.next_message().await {
                embassy_sync::pubsub::WaitResult::Lagged(_count) => {
                    // If lagged, continue to next iteration
                    continue;
                }
                embassy_sync::pubsub::WaitResult::Message(event) => event,
            };
            if (self.filter)(&event) {
                return event;
            }
        }
    }

    /// Try to get next matching event without blocking
    pub fn try_next_event(&mut self) -> Option<SystemEvent> {
        loop {
            match self.inner.try_next_message() {
                Some(wait_result) => {
                    let event = match wait_result {
                        embassy_sync::pubsub::WaitResult::Lagged(_count) => {
                            // If lagged, continue to next iteration
                            continue;
                        }
                        embassy_sync::pubsub::WaitResult::Message(event) => event,
                    };
                    if (self.filter)(&event) {
                        return Some(event);
                    }
                    // Continue loop to check next message
                }
                None => return None,
            }
        }
    }
}

// === CONVENIENCE TRAITS FOR CLEAN INTEGRATION ===

/// Trait for modules that need to publish events
pub trait EventPublishing<'a> {
    fn get_event_publisher(&self) -> &EventPublisher<'a>;
    
    /// Publish with context logging
    async fn publish_with_context(&self, event: SystemEvent, context: &str) {
        log::debug!("📡 [{}] Publishing: {:?}", context, event);
        self.get_event_publisher().publish(event).await;
    }
}

/// Trait for modules that subscribe to events
pub trait EventSubscribing {
    type Subscriber;
    fn get_event_subscriber(&mut self) -> &mut Self::Subscriber;
}
