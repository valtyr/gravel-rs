use crate::types::ScaleData;
use embassy_time::Instant;
use log::{debug, warn};

pub const BLE_SERVICE_UUID: &str = "0000ffe0-0000-1000-8000-00805f9b34fb";
pub const WEIGHT_CHAR_UUID: &str = "0000ff11-0000-1000-8000-00805f9b34fb";  
pub const COMMAND_CHAR_UUID: &str = "0000ff12-0000-1000-8000-00805f9b34fb";

// Convert UUID string to 16-bit UUID for BLE operations
pub const WEIGHT_CHAR_UUID_16: u16 = 0xFF11;
pub const COMMAND_CHAR_UUID_16: u16 = 0xFF12;

pub const TARE_COMMAND: [u8; 6] = [0x03, 0x0A, 0x01, 0x00, 0x00, 0x08];
pub const START_TIMER_COMMAND: [u8; 6] = [0x03, 0x0A, 0x04, 0x00, 0x00, 0x0A];
pub const STOP_TIMER_COMMAND: [u8; 6] = [0x03, 0x0A, 0x05, 0x00, 0x00, 0x0D];
pub const RESET_TIMER_COMMAND: [u8; 6] = [0x03, 0x0A, 0x06, 0x00, 0x00, 0x0C];

fn calculate_xor_checksum(data: &[u8]) -> u8 {
    data.iter().fold(0, |acc, &byte| acc ^ byte)
}

fn verify_checksum(data: &[u8]) -> bool {
    if data.len() < 2 {
        return false;
    }
    
    let payload = &data[..data.len() - 1];
    let expected_checksum = data[data.len() - 1];
    let calculated_checksum = calculate_xor_checksum(payload);
    
    calculated_checksum == expected_checksum
}

pub fn parse_scale_data(data: &[u8]) -> Option<ScaleData> {
    debug!("Parsing scale data: {:02X?}", data);
    
    // Python implementation expects exactly 20 bytes with header [0x03, 0x0B]
    if data.len() != 20 {
        warn!("Invalid data length: expected 20, got {} (Python expects exactly 20)", data.len());
        warn!("Received data: {:02X?}", data);
        warn!("This suggests we're reading from the wrong characteristic - need to find 0xFF11 UUID");
        return None;
    }
    
    if data[0] != 0x03 || data[1] != 0x0B {
        warn!("Invalid header: expected [0x03, 0x0B], got [{:02X}, {:02X}]", data[0], data[1]);
        warn!("This suggests we're reading from the wrong characteristic - need to find 0xFF11 UUID");
        return None;
    }
    
    if !verify_checksum(data) {
        warn!("Checksum verification failed");
        return None;
    }
    
    // Parse timestamp (3 bytes, big endian in Python implementation)
    let timestamp_ms = ((data[2] as u32) << 16) | ((data[3] as u32) << 8) | (data[4] as u32);
    
    // Parse weight with sign (Python implementation)
    let weight_sign = if data[6] == 0x2B { 1.0 } else { -1.0 }; // 0x2B = '+', 0x2D = '-'
    let weight_raw = ((data[7] as u32) << 16) | ((data[8] as u32) << 8) | (data[9] as u32);
    let weight_g = (weight_raw as f32 / 100.0) * weight_sign;
    
    // Parse flow rate with sign (Python implementation)
    let flow_sign = if data[10] == 0x2B { 1.0 } else { -1.0 }; // 0x2B = '+', 0x2D = '-'
    let flow_raw = ((data[11] as u16) << 8) | (data[12] as u16);
    let flow_rate_g_per_s = (flow_raw as f32 / 100.0) * flow_sign;
    
    let battery_percent = data[13];
    
    // Timer state is determined by analyzing timestamp changes over time,
    // not from a specific byte. This should be handled in the controller.
    // For now, parse the raw timestamp and let the controller determine timer state.
    let timer_running = timestamp_ms > 0; // Basic heuristic: timer running if timestamp > 0
    
    Some(ScaleData {
        timestamp_ms,
        weight_g,
        flow_rate_g_per_s,
        battery_percent,
        timer_running,
        received_at: Instant::now(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_checksum_calculation() {
        let data = [0x03, 0x0A, 0x01, 0x00, 0x00];
        let checksum = calculate_xor_checksum(&data);
        assert_eq!(checksum, 0x08);
    }
    
    #[test]
    fn test_checksum_verification() {
        let valid_data = [0x03, 0x0A, 0x01, 0x00, 0x00, 0x08];
        assert!(verify_checksum(&valid_data));
        
        let invalid_data = [0x03, 0x0A, 0x01, 0x00, 0x00, 0x09];
        assert!(!verify_checksum(&invalid_data));
    }
}