# Gravel-RS TODO List

## Completed ✅

1. **Research ESP32-C6 BLE stack compatibility** - Found NimBLE vs Bluedroid issues
2. **Analyze BLE initialization error** - Identified esp32-nimble crate stability issues
3. **Investigate esp-idf-svc feature flags** - Limited BLE support in current version
4. **Find working BLE solution** - esp32-nimble works for scanning but crashes on service discovery
5. **Fix esp32-nimble API usage** - Corrected API patterns from source code
6. **Add Wi-Fi initialization** - Prevents HTTP server networking crashes
7. **Make HTTP server non-fatal** - Allows BLE functionality to continue if web interface fails
8. **Test BLE scanning** - Successfully found and connected to "BOOKOO_SC 353886" scale
9. **Create ble_bindings.rs module** - ✅ Basic structure with placeholders compiles successfully

## High Priority - In Progress 🔧

### Phase 1: Custom BLE Implementation (Day 1-2)

10. **Implement BLE scanning** - ✅ Basic scanning structure with mock device discovery compiles successfully
11. **Implement BLE connection** - Connect to discovered scale device (placeholder implementation)
12. **Implement GATT discovery** - Find services and characteristics safely (placeholder implementation)

### Phase 2: Data Flow (Day 2-3)

13. **Implement characteristic subscription** - Subscribe to weight notifications
14. **Integrate custom BLE client** - ✅ Basic integration complete, test with real hardware

## Medium Priority - Validation & Polish (Day 3-4)

15. **Test end-to-end data flow** - Verify scale data -> parsing -> state management
16. **Test relay control integration** - Verify scale data triggers relay properly
17. **Test auto-tare and overshoot correction** - Overshoot estimates should be persisted to non volatile memory and restored on restart.

---

## Current Status

- **Architecture**: Custom BLE client compiles and integrates with existing code ✅
- **Mock Scanning**: Successfully tested on hardware without crashes ✅
- **Real Scanning**: ESP-IDF NimBLE scanning implemented and working! Found actual scale ✅
- **Configuration**: NimBLE-only config optimized for ESP32-C6 ✅
- **Real Connection**: ESP-IDF NimBLE connection working! Successfully connected to scale ✅
- **GATT Discovery**: Basic service discovery structure implemented with fallback mode ✅
- **Next Step**: Test GATT service discovery on hardware with actual scale connection
- **Goal**: Complete GATT discovery and implement notification subscription for scale data

## Implementation Progress

- ✅ Removed esp32-nimble dependency
- ✅ Created CustomBleClient with Embassy integration
- ✅ Simplified BleScaleClient as wrapper
- ✅ All existing interfaces preserved
- ✅ Implemented real ESP-IDF NimBLE scanning APIs
- ✅ Added proper advertisement data parsing
- ✅ Configured NimBLE-only ESP-IDF setup
- ✅ Implemented real BLE connection with ESP-IDF APIs
- ✅ Added GATT service discovery framework with fallback mode
- ✅ Created notification subscription structure

## Next Immediate Steps

1. **Test GATT service discovery on hardware** - Verify service discovery works with actual scale connection
2. **Implement proper notification subscription** - Use correct ESP-IDF GATT client APIs for notifications
3. **Add scale data parsing and channel integration** - Parse weight data and send to system channels
4. **Validate scanning behavior** - We should scan constantly (or within reason) while there is no scale connected. As soon as we find a scale, we stop scanning and connect to it. After the scale has disconnected we go back to the scanning routine.
