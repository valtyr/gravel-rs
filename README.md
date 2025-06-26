# Espresso Scale Controller

> [!WARNING]  
> I wanted to give vibe coding a try, so this whole thing was built by talking to Claude without ever opening an editor.  
> Any jank you find is purely the AI's fault. I'm a real programmer, I swear.

A Rust-based ESP32-C6 application that interfaces with a Bookoo Themis Mini scale via BLE to control an espresso machine through a relay. This project provides predictive shot control, auto-taring, and a web interface for monitoring and configuration.

## Features

### Core Functionality

- **BLE Scale Integration**: Connects to Bookoo Themis Mini scale using custom protocol
- **Predictive Shot Control**: Uses flow rate data to predict optimal stop timing
- **Auto-Tare System**: Automatically tares scale when objects are placed/removed
- **Relay Control**: Direct GPIO control (GPIO19) for espresso machine
- **Safety Systems**: Multiple failsafes to prevent equipment damage

### Web Interface

- **Real-time Monitoring**: Live weight, flow rate, and system status
- **Configuration**: Adjust target weight, auto-tare, and other settings
- **Manual Controls**: Tare scale, start/stop timer, test relay
- **System Health**: BLE/Wi-Fi status, error messages, and logs

### Advanced Features

- **Overshoot Learning**: Adaptive compensation for brewing delays
- **State Management**: Comprehensive brewing state tracking
- **Multi-tasking**: Concurrent BLE, Wi-Fi, and control operations
- **Robust Error Handling**: Graceful degradation and recovery

## Hardware Requirements

- **ESP32-C6** development board
- **Bookoo Themis Mini** scale
- **Relay module** connected to GPIO19 (active high) for espresso machine control
- **Wi-Fi network** for web interface access

## Setup Instructions

### 1. Environment Setup

```bash
# Install Rust and ESP-IDF toolchain
cargo install espup
espup install
. $HOME/export-esp.sh

# Install additional tools
cargo install cargo-espflash
```

### 2. Configuration

Edit `sdkconfig.defaults` to configure:

- Wi-Fi credentials (if using hardcoded values)
- BLE parameters
- Memory allocation settings

### 3. Build and Flash

```bash
# Build the project
cargo build --release

# Flash to ESP32-C6
cargo espflash flash --release --port /dev/ttyUSB0

# Monitor serial output
cargo espflash monitor --port /dev/ttyUSB0
```

### 4. Web Interface Access

1. Find the ESP32-C6's IP address from serial output
2. Open `http://[ESP_IP]:8081` in your browser
3. The WebSocket connection uses port 8080

## Usage

### Initial Setup

1. Power on the ESP32-C6
2. Ensure the Bookoo scale is discoverable (power cycle if needed)
3. Access the web interface to configure settings

### Brewing Process

1. Place cup on scale (auto-tare will activate)
2. The system automatically detects timer start from scale
3. Predictive stopping occurs based on flow rate analysis
4. System returns to idle state after brewing

### Manual Controls

- **Tare Scale**: Zero the scale reading
- **Start/Stop Timer**: Manual timer control
- **Test Relay**: Test GPIO relay functionality
- **Reset Overshoot**: Clear adaptive learning data
- **Configuration**: Adjust target weight and auto-tare settings

## Architecture

### Module Structure

```
src/
├── main.rs           # Application entry point
├── lib.rs            # Module declarations
├── controller.rs     # Main application controller
├── ble.rs            # BLE scale communication
├── websocket.rs      # WebSocket server and web UI
├── relay.rs          # HTTP relay control
├── state.rs          # System state management
├── safety.rs         # Safety and watchdog systems
├── protocol.rs       # BLE protocol implementation
├── auto_tare.rs      # Auto-tare state machine
├── brew_states.rs    # Brewing state tracking
├── overshoot.rs      # Predictive control and learning
└── types.rs          # Common data structures
```

### Key Components

**EspressoController**: Main orchestrator managing all subsystems
**BleScaleClient**: Handles BLE connection and protocol communication
**WebSocketServer**: Provides real-time web interface
**RelayController**: Manages GPIO-based relay control (GPIO19)
**SafetyController**: Implements multiple safety mechanisms
**StateManager**: Centralized state management with thread-safe access

### Communication Flow

1. BLE scale data → Protocol parsing → State updates
2. State changes → Brewing logic → Relay control
3. Web interface ← WebSocket ← State updates
4. User commands → WebSocket → Controller actions

## BLE Protocol

The system implements the Bookoo Themis Mini's proprietary BLE protocol:

- **Service UUID**: `0000ffe0-0000-1000-8000-00805f9b34fb`
- **Weight Data**: `0000ff11-0000-1000-8000-00805f9b34fb`
- **Commands**: `0000ff12-0000-1000-8000-00805f9b34fb`

### Supported Commands

- Tare: `[0x03, 0x0A, 0x01, 0x00, 0x00, 0x08]`
- Start Timer: `[0x03, 0x0A, 0x04, 0x00, 0x00, 0x0A]`
- Stop Timer: `[0x03, 0x0A, 0x05, 0x00, 0x00, 0x0D]`
- Reset Timer: `[0x03, 0x0A, 0x06, 0x00, 0x00, 0x0C]`

## Safety Features

### Critical Safety Systems

1. **Emergency Stop**: Immediate relay shutdown on any fault
2. **BLE Watchdog**: Monitors connection and data flow
3. **Timer Validation**: Ensures relay state matches timer state
4. **Error Recovery**: Automatic retry and graceful degradation

### Fail-Safe Conditions

- BLE disconnection during brewing
- Network connectivity loss
- Data parsing errors
- Watchdog timeouts
- Manual emergency stop

## Configuration Options

### Default Settings

- **Target Weight**: 36.0g
- **Auto-Tare**: Enabled
- **Predictive Stop**: Enabled
- **Relay GPIO**: GPIO19 (active high)

### Adjustable Parameters

- Target weight (1-100g)
- Auto-tare sensitivity and timing
- Predictive stop margins
- Safety timeout values

## Troubleshooting

### Common Issues

1. **Scale Not Found**: Ensure scale is powered and in pairing mode
2. **Web Interface Unreachable**: Check Wi-Fi connection and IP address
3. **Relay Not Responding**: Check GPIO19 wiring and use test relay function
4. **Predictive Stop Issues**: Reset overshoot learning data

### Debug Information

- Serial console provides detailed logging
- Web interface shows system status and recent log messages
- BLE connection status and data flow indicators

## Development

### Build Commands

```bash
# Debug build
cargo build

# Release build (optimized for size)
cargo build --release

# Check without building
cargo check

# Format code
cargo fmt

# Run clippy lints
cargo clippy
```

### Testing

```bash
# Run unit tests
cargo test

# Run with specific ESP-IDF target
cargo build --target xtensa-esp32s3-espidf
```

## License

This project is licensed under the MIT License - see the LICENSE file for details.

## Acknowledgments

- Bookoo Coffee for the Themis Mini scale and protocol documentation
- ESP-RS community for ESP32 Rust tooling
- Embassy for async embedded framework
