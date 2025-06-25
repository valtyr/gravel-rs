# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

This is an ESP32-C6 Rust project implementing an espresso scale controller. It connects to a Bookoo Themis Mini scale via BLE and controls an espresso machine via relay, with a web interface for monitoring and control.

## Development Environment

- **Rust Toolchain**: Nightly channel with `rust-src` component
- **Target Platform**: ESP32-C6 (ESP-IDF)
- **Async Framework**: Embassy with Tokio runtime
- **Main Dependencies**: `esp-idf-svc`, `embassy-*`, `tokio`, `serde`

## Build Commands

```bash
# Build the project
cargo build

# Build for release (optimized for size)
cargo build --release

# Flash to ESP32-C6 device
cargo espflash flash --release --port /dev/ttyUSB0

# Monitor serial output
cargo espflash monitor --port /dev/ttyUSB0

# Check code without building
cargo check

# Format and lint
cargo fmt && cargo clippy
```

## Architecture Overview

This is a multi-threaded async application with several concurrent tasks:

### Core Modules
- **controller.rs**: Main application orchestrator (`EspressoController`)
- **ble.rs**: BLE scale communication (`BleScaleClient`)
- **websocket.rs**: WebSocket server and HTTP web interface
- **relay.rs**: GPIO relay control (GPIO19) with safety mechanisms
- **state.rs**: Thread-safe centralized state management
- **safety.rs**: Critical safety systems and emergency stop logic

### Protocol and Logic
- **protocol.rs**: Bookoo scale BLE protocol implementation
- **auto_tare.rs**: Object detection and automatic taring state machine
- **brew_states.rs**: Brewing lifecycle tracking (Idle/Brewing/Settling)
- **overshoot.rs**: Predictive control with adaptive learning
- **types.rs**: Shared data structures and constants

## Key Configuration Details

- **ESP32-C6 Target**: Configured in `sdkconfig.defaults`
- **BLE + Wi-Fi Coexistence**: Enabled for simultaneous operation
- **Memory**: SPIRAM support enabled for larger heap
- **Async Runtime**: Uses Embassy for concurrent operations (NOT Tokio)
- **Safety**: Multiple watchdog systems and fail-safe mechanisms

## Development Guidelines

### Safety-Critical Code
- **Relay Control**: Always implement emergency stop capability
- **BLE Watchdog**: Monitor connection and data flow continuously
- **State Validation**: Ensure system state consistency at all times
- **Error Handling**: Graceful degradation, never leave relay on during faults

### Async Patterns - IMPORTANT
- **Use Embassy, NOT Tokio**: This is an embedded project - use `embassy_futures::select!` not `tokio::select!`
- **Proper ESP-IDF Configuration**: Enable features in `sdkconfig.defaults` instead of stubbing code
- **Embassy Tasks**: Use `#[embassy_executor::task]` for concurrent tasks
- **Embassy Channels**: Use `embassy_sync::Channel` for inter-task communication
- **ESP-IDF Features**: Enable BLE (`CONFIG_BT_ENABLED=y`), WebSocket (`CONFIG_HTTPD_WS_SUPPORT=y`) properly

### BLE Protocol Notes
- **Service UUID**: `0000ffe0-0000-1000-8000-00805f9b34fb`
- **Weight Data**: 20-byte packets with XOR checksum
- **Commands**: 6-byte command structure with XOR verification
- **Connection**: Handle disconnects gracefully, implement reconnection logic

### Testing and Debugging
- Use serial console for real-time debugging (`log` crate)
- Web interface provides system status and diagnostics
- Test safety systems regularly (emergency stop, watchdog)
- Validate BLE protocol parsing with known good data

## Common Patterns

### Adding New WebSocket Commands
1. Add variant to `WebSocketCommand` enum in `websocket.rs`
2. Handle parsing in `handle_websocket_message()`
3. Implement logic in `EspressoController::handle_websocket_command()`
4. Update web interface JavaScript and HTML

### Modifying Safety Logic
1. Update conditions in `SafetyController::should_emergency_stop()`
2. Ensure `emergency_stop()` is called immediately on fault detection
3. Test all failure modes thoroughly
4. Document safety changes in commit messages

### State Management
- All state changes should go through `StateManager`
- Use appropriate async locks (`Mutex::lock().await`)
- Log significant state transitions
- Maintain state consistency across all modules

## Important Notes

- **Never bypass safety systems** - always implement proper error handling
- **BLE can disconnect anytime** - design for robustness
- **Relay control is safety-critical** - implement immediate emergency stop
- **Embassy ONLY** - this is embedded, don't use Tokio
- **Enable ESP-IDF features properly** - use `sdkconfig.defaults` instead of stubbing code
- **Memory constraints** - optimize for embedded use (heapless collections)

## Common Mistakes to Avoid

### DON'T stub out implementations
❌ Bad: Commenting out functionality with "simplified for build compatibility"
✅ Good: Enable proper ESP-IDF features in `sdkconfig.defaults`

### DON'T use Tokio in embedded
❌ Bad: `tokio::select!`, `tokio::spawn`, `#[tokio::main]`
✅ Good: `embassy_futures::select!`, `embassy_executor::Spawner`, `#[embassy_executor::main]`

### DON'T skip configuration
❌ Bad: Stubbing BLE because imports don't work
✅ Good: Enable `CONFIG_BT_ENABLED=y`, `CONFIG_BT_BLUEDROID_ENABLED=y` in sdkconfig

## ESP-IDF Specific Notes

- `esp_idf_svc::sys::link_patches()` must be called once for proper runtime linking
- Uses ESP-IDF's native logging facilities through `EspLogger::initialize_default()`
- BLE and Wi-Fi coexistence requires proper configuration in `sdkconfig.defaults`
- Embassy time driver integration requires careful task scheduling