// Simplified JavaScript for ESP32 HTTP server
// In a full implementation, this would use WebSockets

// Simulate some basic functionality
let mockState = {
    scale_weight: 0.0,
    target_weight: 36.0,
    flow_rate: 0.0,
    timer_state: 'Idle',
    ble_connected: false,
    wifi_connected: true,
    relay_enabled: false,
    brew_state: 'Idle'
};

function updateUI() {
    document.getElementById('scale-weight').textContent = mockState.scale_weight.toFixed(2);
    document.getElementById('target-weight').textContent = mockState.target_weight.toFixed(1);
    document.getElementById('flow-rate').textContent = mockState.flow_rate.toFixed(2);
    document.getElementById('timer-state').textContent = mockState.timer_state;
    document.getElementById('ble-status').textContent = mockState.ble_connected ? 'Connected' : 'Disconnected';
    document.getElementById('wifi-status').textContent = mockState.wifi_connected ? 'Connected' : 'Disconnected';
    document.getElementById('relay-status').textContent = mockState.relay_enabled ? 'ON' : 'OFF';
    document.getElementById('brew-state').textContent = mockState.brew_state;
}

function addLogMessage(message) {
    const logContainer = document.getElementById('log-messages');
    const timestamp = new Date().toLocaleTimeString();
    const div = document.createElement('div');
    div.textContent = `[${timestamp}] ${message}`;
    logContainer.appendChild(div);
    logContainer.scrollTop = logContainer.scrollHeight;
}

function setTargetWeight() {
    const weight = parseFloat(document.getElementById('target-weight-input').value);
    mockState.target_weight = weight;
    updateUI();
    addLogMessage(`Target weight set to ${weight.toFixed(1)}g`);
}

function testRelay() {
    addLogMessage('Testing relay (GPIO19)...');
    mockState.relay_enabled = true;
    updateUI();
    setTimeout(() => {
        mockState.relay_enabled = false;
        updateUI();
        addLogMessage('Relay test completed');
    }, 1000);
}

function tareScale() {
    addLogMessage('Taring scale...');
    mockState.scale_weight = 0.0;
    updateUI();
}

function startTimer() {
    addLogMessage('Starting timer...');
    mockState.timer_state = 'Running';
    mockState.relay_enabled = true;
    updateUI();
}

function stopTimer() {
    addLogMessage('Stopping timer...');
    mockState.timer_state = 'Idle';
    mockState.relay_enabled = false;
    updateUI();
}

function resetTimer() {
    addLogMessage('Resetting timer...');
    mockState.timer_state = 'Idle';
    mockState.relay_enabled = false;
    updateUI();
}

function resetOvershoot() {
    addLogMessage('Resetting overshoot learning data...');
}

// Auto-update checkboxes
document.getElementById('auto-tare-checkbox').addEventListener('change', function() {
    addLogMessage(`Auto-tare: ${this.checked ? 'enabled' : 'disabled'}`);
});

document.getElementById('predictive-stop-checkbox').addEventListener('change', function() {
    addLogMessage(`Predictive stop: ${this.checked ? 'enabled' : 'disabled'}`);
});

// Initialize UI on page load
updateUI();
addLogMessage('Web interface loaded');
addLogMessage('Note: This is a demo interface - full WebSocket implementation would provide real-time data');