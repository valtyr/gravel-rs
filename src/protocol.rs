use crate::types::ScaleData;
use embassy_time::Instant;
use log::{debug, warn};

pub const BLE_SERVICE_UUID: &str = "0000ffe0-0000-1000-8000-00805f9b34fb";
pub const WEIGHT_CHAR_UUID: &str = "0000ff11-0000-1000-8000-00805f9b34fb";  
pub const COMMAND_CHAR_UUID: &str = "0000ff12-0000-1000-8000-00805f9b34fb";

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
    
    if data.len() != 20 {
        warn!("Invalid data length: expected 20, got {}", data.len());
        return None;
    }
    
    if data[0] != 0x03 || data[1] != 0x0B {
        warn!("Invalid header: expected [0x03, 0x0B], got [{:02X}, {:02X}]", data[0], data[1]);
        return None;
    }
    
    if !verify_checksum(data) {
        warn!("Checksum verification failed");
        return None;
    }
    
    let timestamp_ms = u32::from_le_bytes([data[2], data[3], data[4], data[5]]);
    
    let weight_raw = i16::from_le_bytes([data[6], data[7]]) as i32;
    let weight_g = weight_raw as f32 / 100.0;
    
    let flow_rate_raw = i16::from_le_bytes([data[8], data[9]]) as i32;
    let flow_rate_g_per_s = flow_rate_raw as f32 / 100.0;
    
    let battery_percent = data[10];
    
    let timer_running = data[11] != 0;
    
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