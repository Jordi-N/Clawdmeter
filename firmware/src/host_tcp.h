#pragma once
#include <stdint.h>

// TCP side of the WiFi transport: a non-blocking BSD-socket server (lwIP on
// the ESP32, POSIX in the simulator) holding ONE authenticated client.
// Framing and auth rules: wifi_proto.h, docs/protocol.md "WiFi transport".
// Only host_wifi.cpp and the sim's radio call it; main.cpp goes through
// host_wifi.h.

bool tcp_host_start(uint16_t port);
void tcp_host_stop(void);           // closes the listener and every client
bool tcp_host_running(void);
// Accepts, authenticates against `key` (32 hex) and reads. Each line from
// the authenticated client goes to on_line. Never blocks.
void tcp_host_tick(const char* key, void (*on_line)(const char* line));
bool tcp_host_authed(void);
// Writes `line` + '\n' to the authenticated client (dropped if none or if
// the socket buffer is full).
void tcp_host_send(const char* line);
void tcp_host_drop_clients(void);   // e.g. after the key changed

// Counters since boot, for host_wifi_diag().
struct TcpStats {
    uint32_t accepts;       // TCP connections accepted
    uint32_t authed;        // handshakes that succeeded
    uint32_t auth_fail;     // bad hello or bad auth MAC
    uint32_t auth_timeout;  // no auth within AUTH_TIMEOUT_MS
    uint32_t closed;        // authenticated host closed the socket
    uint32_t idle;          // authenticated host dropped for silence
    uint32_t write_fail;    // send failed or short: client dropped
    uint32_t last_line_ms;  // last line from the authenticated host, 0 none
};
const TcpStats* tcp_host_stats(void);
