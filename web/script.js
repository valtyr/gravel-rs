// Espresso Scale Controller HTTP Polling Client
// Real-time connection to ESP32 via 5Hz HTTP polling

class EspressoWebClient {
    constructor() {
        this.pollingInterval = null;
        this.pollingRate = 200; // 5Hz (200ms)
        this.state = {
            scale_weight: 0.0,
            target_weight: 36.0,
            flow_rate: 0.0,
            timer_state: 'Idle',
            ble_connected: false,
            wifi_connected: true,
            relay_enabled: false,
            brew_state: 'Idle',
            battery_percent: 0,
            auto_tare_enabled: true,
            predictive_stop_enabled: true,
            overshoot_info: 'No data',
            error: null
        };
        this.initPolling();
    }

    initPolling() {
        addLogMessage('🔄 Starting HTTP polling at 5Hz (200ms intervals)');
        
        // Start immediate poll
        this.pollServer();
        
        // Set up polling interval
        this.pollingInterval = setInterval(() => {
            this.pollServer();
        }, this.pollingRate);
        
        addLogMessage('✅ HTTP polling started - real-time data active');
    }

    async pollServer() {
        try {
            const response = await fetch('/state', {
                method: 'GET',
                headers: {
                    'Cache-Control': 'no-cache'
                }
            });
            
            if (response.ok) {
                const data = await response.json();
                this.handleServerMessage(data);
            } else {
                console.warn(`Polling failed: ${response.status} ${response.statusText}`);
                // Don't spam logs for temporary failures
            }
        } catch (error) {
            console.warn(`Polling error: ${error.message}`);
            // Don't spam logs for network errors
        }
    }

    stopPolling() {
        if (this.pollingInterval) {
            clearInterval(this.pollingInterval);
            this.pollingInterval = null;
            addLogMessage('⏹️ HTTP polling stopped');
        }
    }

    handleServerMessage(data) {
        // Skip welcome message handling (not needed for polling)
        
        // Update scale data if present
        if (data.scale_data) {
            this.state.scale_weight = data.scale_data.weight_g;
            this.state.flow_rate = data.scale_data.flow_rate_g_per_s;
            this.state.battery_percent = data.scale_data.battery_percent;
        }

        // Update system state
        if (data.system_state) {
            const sys = data.system_state;
            this.state.timer_state = sys.timer_state;
            this.state.brew_state = sys.brew_state;
            this.state.target_weight = sys.target_weight_g;
            this.state.ble_connected = sys.ble_connected;
            this.state.relay_enabled = sys.relay_enabled;
            this.state.auto_tare_enabled = sys.auto_tare_enabled;
            this.state.predictive_stop_enabled = sys.predictive_stop_enabled;
            this.state.overshoot_info = sys.overshoot_info;
            this.state.error = sys.error;
        }

        this.updateUI();
    }

    sendCommand(command) {
        fetch('/command', {
            method: 'POST',
            headers: {
                'Content-Type': 'application/json',
            },
            body: JSON.stringify(command)
        })
        .then(response => {
            if (response.ok) {
                addLogMessage(`📤 Sent: ${command.type}`);
            } else {
                addLogMessage(`❌ Command failed: ${command.type}`);
            }
        })
        .catch(error => {
            addLogMessage(`❌ Command error: ${error.message}`);
        });
    }

    updateUI() {
        document.getElementById('scale-weight').textContent = this.state.scale_weight.toFixed(2);
        document.getElementById('target-weight').textContent = this.state.target_weight.toFixed(1);
        document.getElementById('flow-rate').textContent = this.state.flow_rate.toFixed(2);
        document.getElementById('timer-state').textContent = this.state.timer_state;
        document.getElementById('battery-level').textContent = this.state.battery_percent + '%';
        document.getElementById('ble-status').textContent = this.state.ble_connected ? 'Connected' : 'Disconnected';
        document.getElementById('wifi-status').textContent = 'Connected'; // We're getting data, so WiFi works
        document.getElementById('relay-status').textContent = this.state.relay_enabled ? 'ON' : 'OFF';
        document.getElementById('brew-state').textContent = this.state.brew_state;
        document.getElementById('overshoot-info').textContent = this.state.overshoot_info;

        // Update checkboxes to match server state
        document.getElementById('auto-tare-checkbox').checked = this.state.auto_tare_enabled;
        document.getElementById('predictive-stop-checkbox').checked = this.state.predictive_stop_enabled;
        
        // Only update target weight input if it's not currently focused (user isn't typing)
        const targetInput = document.getElementById('target-weight-input');
        if (document.activeElement !== targetInput) {
            targetInput.value = this.state.target_weight;
        }

        // Add visual indicators for connection status
        this.updateStatusColors();

        // Show error if present
        if (this.state.error) {
            addLogMessage(`⚠️ System error: ${this.state.error}`);
        }
    }

    updateStatusColors() {
        // Color-code BLE status
        const bleStatus = document.getElementById('ble-status');
        bleStatus.style.color = this.state.ble_connected ? '#28a745' : '#dc3545';
        
        // Color-code relay status
        const relayStatus = document.getElementById('relay-status');
        relayStatus.style.color = this.state.relay_enabled ? '#ffc107' : '#6c757d';
        
        // Color-code battery level
        const batteryLevel = document.getElementById('battery-level');
        if (this.state.battery_percent > 50) {
            batteryLevel.style.color = '#28a745';
        } else if (this.state.battery_percent > 20) {
            batteryLevel.style.color = '#ffc107';
        } else {
            batteryLevel.style.color = '#dc3545';
        }
    }
}

// Global client instance
let client = null;

function addLogMessage(message) {
    const logContainer = document.getElementById('log-messages');
    const timestamp = new Date().toLocaleTimeString();
    const div = document.createElement('div');
    div.textContent = `[${timestamp}] ${message}`;
    logContainer.appendChild(div);
    
    // Keep only the last 150 messages to prevent memory buildup
    const maxMessages = 150;
    while (logContainer.children.length > maxMessages) {
        logContainer.removeChild(logContainer.firstChild);
    }
    
    logContainer.scrollTop = logContainer.scrollHeight;
}

// Command functions - send to ESP32 via WebSocket
function setTargetWeight() {
    const weight = parseFloat(document.getElementById('target-weight-input').value);
    if (isNaN(weight) || weight <= 0) {
        addLogMessage('❌ Invalid target weight');
        return;
    }
    
    client.sendCommand({
        type: 'set_target_weight',
        weight: weight
    });
}

function testRelay() {
    client.sendCommand({
        type: 'test_relay'
    });
}

function tareScale() {
    client.sendCommand({
        type: 'tare_scale'
    });
}

function startTimer() {
    client.sendCommand({
        type: 'start_timer'
    });
}

function stopTimer() {
    client.sendCommand({
        type: 'stop_timer'
    });
}

function resetTimer() {
    client.sendCommand({
        type: 'reset_timer'
    });
}

function resetOvershoot() {
    client.sendCommand({
        type: 'reset_overshoot'
    });
}

// Auto-update checkboxes - send to server
document.getElementById('auto-tare-checkbox').addEventListener('change', function() {
    client.sendCommand({
        type: 'set_auto_tare',
        enabled: this.checked
    });
});

document.getElementById('predictive-stop-checkbox').addEventListener('change', function() {
    client.sendCommand({
        type: 'set_predictive_stop',
        enabled: this.checked
    });
});

// Initialize HTTP polling client on page load
document.addEventListener('DOMContentLoaded', function() {
    client = new EspressoWebClient();
    addLogMessage('🚀 Espresso Scale Controller - Real-time HTTP polling interface');
    addLogMessage('📡 Connecting to ESP32 via 5Hz polling...');
});

// Clean up polling on page unload
window.addEventListener('beforeunload', function() {
    if (client) {
        client.stopPolling();
    }
});