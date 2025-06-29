# Gravel - Intelligent Espresso Scale Controller

An ESP32-C6 Rust application that provides automated, weight-based espresso shot control by integrating with a Bookoo Themis Mini scale via BLE and controlling an espresso machine through relay switching.

## Overview

Gravel transforms any espresso machine into a smart brewing system by:
- **BLE Scale Integration**: Real-time weight and flow rate data from Bookoo scale
- **Predictive Stopping**: Advanced flow analysis with adaptive overshoot compensation  
- **Auto-Tare Detection**: Intelligent object placement/removal detection
- **Web Interface**: Real-time monitoring and control via WiFi
- **Event-Driven Architecture**: Unified state machine managing all brewing operations

## Quick Start

```bash
# Flash and run
cargo espflash flash --release --port /dev/ttyUSB0
cargo espflash monitor --port /dev/ttyUSB0

# Access web interface at http://[ESP_IP]:8081
```

## Hardware Requirements

- **ESP32-C6** development board
- **Bookoo Themis Mini** smart scale  
- **Relay module** (GPIO19, active high)
- **WiFi network** for web interface

## Repository Structure

### Core Application (`src/`)

```
src/
├── main.rs              # Application entry point
├── lib.rs               # Module declarations  
├── controller.rs        # Main system orchestrator
├── state.rs             # Thread-safe state management
├── types.rs             # Shared data structures
```

### Hardware Integration (`src/hardware/`)

```
hardware/
├── mod.rs              # Hardware module exports
├── relay.rs            # GPIO relay control (GPIO19)
└── display.rs          # Future display support
```

### Scale Integration (`src/scales/`)

```
scales/
├── mod.rs              # Scale module exports
├── bookoo.rs           # Bookoo Themis Mini implementation
├── protocol.rs         # BLE protocol parsing
├── traits.rs           # Scale abstraction layer
├── event_detection.rs  # Scale button/timer detection
└── simple_scanner.rs   # Generic BLE scale discovery
```

### Brewing Logic (`src/brewing/`)

```
brewing/
├── mod.rs              # Brewing module exports
├── controller.rs       # Brewing state machine controller
├── states.rs           # Comprehensive state machine (statig-based)
├── auto_tare.rs        # Auto-tare state management
└── overshoot.rs        # Predictive control algorithms
```

### System Management (`src/system/`)

```
system/
├── mod.rs              # System module exports
├── events.rs           # Event bus and system events
├── safety.rs           # Safety controllers and emergency stop
├── storage.rs          # NVS persistent storage
└── config.rs           # Configuration management
```

### Networking (`src/wifi/` & `src/server/`)

```
wifi/
├── mod.rs              # WiFi module exports
├── manager.rs          # WiFi connection management
└── provisioning.rs     # WiFi credential provisioning

server/
├── mod.rs              # Server module exports
└── http.rs             # HTTP/WebSocket server
```

### BLE Communication (`src/ble.rs`)

Generic BLE client with ESP32-C6 NimBLE integration supporting:
- Device scanning and filtering
- Service/characteristic discovery  
- Notification subscriptions
- Connection management

## Architecture Overview

### Event-Driven State Machine

The core architecture uses a unified state machine (`brewing/states.rs`) with:

- **Pure State Logic**: All decisions made in state machine
- **Side Effect Outputs**: Hardware actions as state machine outputs
- **Event Bus**: Type-safe event communication between components
- **Superstates**: Logical grouping (ScaleConnected, ActiveBrewing, etc.)

```rust
// Example: Scale data flows through state machine
ScaleData -> BrewStateMachine -> [RelayOn, TareRequested] -> Hardware
```

### Key Components

| Component | Purpose | Location |
|-----------|---------|----------|
| `EspressoController` | System orchestrator | `controller.rs` |
| `BrewStateMachine` | Core brewing logic | `brewing/states.rs` |
| `BrewController` | State machine wrapper | `brewing/controller.rs` |
| `BookooScale` | Scale communication | `scales/bookoo.rs` |
| `SimpleScaleScanner` | Scale discovery | `scales/simple_scanner.rs` |
| `EventBus` | System-wide events | `system/events.rs` |
| `StateManager` | Shared state | `state.rs` |
| `RelayController` | Hardware control | `hardware/relay.rs` |

### Data Flow

1. **Scale Data**: BLE → Protocol Parsing → State Machine → Brewing Logic
2. **User Commands**: Web UI → WebSocket → Event Bus → State Machine  
3. **Hardware Control**: State Machine → Side Effects → Relay/BLE Commands
4. **State Updates**: State Machine → State Manager → Web UI (via WebSocket)

## State Machine Architecture

### Core States

- **SystemInit**: Initial startup and hardware detection
- **ScaleConnected**: Scale connected, ready for brewing
- **ActiveBrewing**: Timer running, weight-based control active
- **BrewingComplete**: Settling period after brewing stops

### Advanced Features

- **Auto-Tare State Machine**: Detects object placement/removal patterns
- **Overshoot Control**: EWMA-based learning algorithm for predictive stopping
- **Scale Event Detection**: Infers scale button presses from data patterns
- **Safety Systems**: Multiple watchdogs and emergency stop mechanisms

## Development

### Build Commands

```bash
# Development build
cargo build

# Release build (optimized for ESP32)
cargo build --release

# Check code without building  
cargo check

# Format and lint
cargo fmt && cargo clippy
```

### ESP32 Flashing

```bash
# Flash firmware
cargo espflash flash --release --port /dev/ttyUSB0

# Monitor serial output
cargo espflash monitor --port /dev/ttyUSB0
```

### Configuration Files

- `sdkconfig.defaults`: ESP-IDF configuration (BLE, WiFi, memory)
- `Cargo.toml`: Rust dependencies and ESP32 target configuration
- `CLAUDE.md`: Development guidelines and architecture notes

## BLE Protocol (Bookoo Themis Mini)

- **Service**: `0000ffe0-0000-1000-8000-00805f9b34fb`
- **Weight Data**: `0000ff11-0000-1000-8000-00805f9b34fb` (notifications)
- **Commands**: `0000ff12-0000-1000-8000-00805f9b34fb` (write)

### Scale Commands

| Command | Bytes | Function |
|---------|-------|----------|
| Tare | `[0x03, 0x0A, 0x01, 0x00, 0x00, 0x08]` | Zero scale |
| Start Timer | `[0x03, 0x0A, 0x04, 0x00, 0x00, 0x0A]` | Start brewing timer |
| Stop Timer | `[0x03, 0x0A, 0x05, 0x00, 0x00, 0x0D]` | Stop brewing timer |
| Reset Timer | `[0x03, 0x0A, 0x06, 0x00, 0x00, 0x0C]` | Reset timer to zero |

## Web Interface

Access at `http://[ESP_IP]:8081` for:
- Real-time weight and flow rate monitoring
- Brewing parameter configuration  
- Manual scale commands (tare, timer control)
- System status and diagnostics
- Overshoot learning management

## Safety Features

- **Emergency Stop**: Immediate relay shutdown on any fault condition
- **BLE Watchdog**: Monitors scale connection and data flow
- **State Validation**: Ensures consistent system state
- **Graceful Degradation**: Continues operation with reduced functionality
- **Hardware Fail-Safe**: Machine works normally if ESP32 is disconnected

## Future Extensibility

The architecture supports:
- **Multi-Scale Support**: Generic scanner can detect different scale brands
- **Additional Hardware**: Display modules, sensors, etc.
- **Enhanced Algorithms**: More sophisticated brewing control logic
- **Cloud Integration**: Data logging and remote monitoring
- **Mobile Apps**: Native mobile interfaces

## License

MIT License - See LICENSE file for details.

## Acknowledgments

Built with ESP-RS ecosystem, Embassy async framework, and the Rust embedded community.