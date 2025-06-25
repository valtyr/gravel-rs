Re-implement the following Python prototype in Rust for an ESP32-C6. The original connects to a Bookoo Themis Mini scale via BLE and controls an espresso machine via a relay. The Rust implementation must preserve all existing functionality.

⸻

Context
• The current directory contains a working Rust ESP-IDF project (e.g. using esp-idf-hal).
• Python prototype path:

/Users/valtyr/Universe/Code/bookoo-exploration/bookoo_exploration/main.py

    •	Async (e.g. with embassy) is preferred, but not required.
    •	The scale may disconnect at any time. The ESP will be running continuously; the scale is only powered on during brewing.

⸻

Requirements
• Use Rust + ESP-IDF.
• Enable simultaneous BLE and Wi-Fi (via sdkconfig.defaults).
• Use WebSockets to expose a web interface over Wi-Fi:
• Displays: current weight, target weight, flow rate, auto-tare status, and log messages.
• Allows the user to configure settings (e.g. target weight).
• BLE handling:
• Fully replicate the custom BLE protocol (UUIDs, XOR checksums, commands).
• Gracefully handle disconnects and automatically reconnect when the scale powers on.
• Reference protocol documentation here:
https://github.com/BooKooCode/OpenSource/blob/main/bookoo_mini_scale/protocols.md
• Safety-critical:
If anything goes wrong during brewing (BLE disconnects, parsing error, Wi-Fi loss, etc.), the relay must be turned off immediately.
The espresso machine must never remain on under fault conditions.
• Match the relay logic and control behavior from the prototype exactly.
• Maintain a modular structure:
• Clean separation between BLE, WebSocket server, control logic, and configuration/state.
• No functionality should be removed or omitted from the prototype.

⸻

Documentation
• Maintain an up-to-date README.md that documents:
• Project purpose, setup instructions, features, and usage.
• Any configuration options or exposed endpoints.
• Maintain a CLAUDE.md or CHANGELOG.md file:
• Record key decisions, architecture choices, and significant changes.
• Always check this before introducing sweeping changes to avoid regressions or loops.

⸻

Getting Started

Start by outlining architecture and modules, then implement progressively with clear commits and doc updates.
